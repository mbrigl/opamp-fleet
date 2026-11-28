# OPS-operator-program-fleetctl: One operator program, `opamp-fleetctl`, makes the fleet's certificates and keys and repacks upstream trees

- **Status:** 🟢 accepted
- **Date:** 2026-11-15
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-tools/` (the one program it builds, its `pki` commands in `src/pki.rs`, `pki token-init` among them in `src/token.rs`, `p12-extract`, and the deterministic repack of `package fetch` in `src/fetch.rs`, `pack_tree` and `collect_members`, its numbering of a later repack, and its verification of an upstream's OpenPGP signature in `src/gpgv.rs`), the dependencies of `crates/fleet-tools/Cargo.toml`, `cryptoki` among them, and their entries in `deny.toml`, the Server's `pki status` command in `crates/fleet-server/src/main.rs` and the module that carries it, the expiry warnings of the running Server, the leaf naming of host and bootstrap certificates in `crates/fleet-server/src/ca.rs` and its startup check, how the Server holds its CA keys, in a file or on a PKCS#11 token, the ending rule, the reading of a certificate's end with the line `pki status` prints for it, the naming rule in `crates/fleet-core/`, the kept-link rule in `crates/fleet-util/src/tree.rs`, the operator-tool build and its archives in `.github/workflows/release.yml`, the release notes, `scripts/dev-pki.sh` and what reads its output (`scripts/seed_test_configs.sh`, `.vscode/launch.json`, `.vscode/tasks.json`, the startup message of `crates/fleet-server/src/config.rs` that names it), every kind whose upstream tree the repack packs, and every document that names an operator command, tells an operator how to make, inspect or convert a certificate, or says what a repacked package carries

## Context

An Agent proves fleet membership with a client certificate in the TLS handshake and nothing else.
The Server presents a certificate of its own on both planes
([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md),
[SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md)).
That takes three certificate authorities, each with one job:

- the **server CA** signs the Server's certificate and a Gateway's downstream certificate;
- the **client CA** is the one the Server signs every host's certificate with;
- the **bootstrap CA** signs the certificates a new host enrols with
  ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)).

The Server makes every host certificate itself, with `rcgen`, and issues every bootstrap
certificate. A host obtains its first host certificate only through an enrolment an operator opens
and approves. The key of a host's client certificate is made on the host; the bootstrap key is the
one key the Server makes, good only to ask for enrolment. Everything before enrolment is the
operator's: the three CAs, the Server's certificate and a Gateway's certificate.

The certificates are one of three things a fleet needs before it does anything useful; a package
signing key and signed package artifacts are the others (G-10, Q-1). Packing and signing an
artifact and fetching a release of a known agent are commands of the same operator program. Unless
a release carries that program, an operator who wants to roll out a single package needs a
checkout of this repository and a Rust toolchain.

The forces:

- **G-17** asks for a secured connection and an identified Agent, and **Q-1** for a system that is
  secure whatever its configuration. A fleet whose first step is a hand-written `openssl` command
  line is secure only as far as that command line is right. A missing `extendedKeyUsage`, a CA key
  in SEC1 form the Server cannot load, a bootstrap CA that shares its subject with the client CA,
  or a server certificate that names the wrong host each fail later and elsewhere.
- **The requirements are the project's own and are scattered.** The client CA's key must be
  PKCS#8, because `rcgen` loads nothing else. The bootstrap CA must have a subject of its own, or
  the Server refuses to start. The server certificate needs the loopback among its names, so that
  the loopback Operator plane is reachable through a tunnel. A tool that knows these rules writes
  them once; a manual can only list them.
- **`openssl` would be a system dependency of the setup.** The build keeps system libraries out
  ([SEC-bootstrap-certificates-and-enrolment §13](SEC-bootstrap-certificates-and-enrolment.md)),
  and the operator's machine need not carry one either. Its command-line behaviour differs between
  OpenSSL 1.1, 3.x and LibreSSL. OpenSSL 1.1 cannot read the PBES2/AES `bootstrap.p12` the Server
  hands out at all, and OpenSSL 3 needs the right options to open it.
- **Not every key belongs on the Server, and not every key off it.** Whoever holds the server CA's
  key can stand up a Server or a Gateway anywhere that every host trusts, so that key stays on the
  operator's machine. The client CA's key has to be on the Server, because it signs renewals
  unattended. The bootstrap CA's key beside it adds nothing an attacker on the Server host does
  not already have: a bootstrap certificate opens nothing without an open window and an approval.
- **A CA key the Server reads from a file can be copied.** Whoever reads the client CA's key once,
  from the host, a backup or a disk snapshot, can sign a member certificate for any host until the
  CA is replaced. So the Server may hold its CA keys on a PKCS#11 token
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). Clauses 5 to 8,
  12, 13 and 16 to 18 and the out-of-scope list carry the operator side of that: `pki token-init`,
  `pki bootstrap-ca --pkcs11-module`, `pki init --ca-keys-on-token`, the flags of `token-init` and
  `cryptoki`.
- **A CA lives as long as what replacing it costs justifies.** Replacing the server CA means a new
  `ca_file` on every host; replacing the client CA means every host enrolling again. Replacing the
  bootstrap CA touches only `server.toml` and the bootstrap certificates not yet used. It can be
  replaced every year at little cost, and a key that is online is then replaced on a schedule
  rather than only after a compromise.
- **A CA's end is not enforced where it would show first.** `rustls` treats a configured CA as a
  trust anchor: a name and a key without a validity. A Server and its Clients therefore keep
  accepting certificates under an expired CA. A browser on the Operator plane, `curl`, or another
  OpAMP implementation may refuse what this project's own ends accept. Without a warning, the first
  sign of an expired CA or Server certificate is an outage somewhere else.
- **An audit reads certificates, not configuration.** An auditor shown a certificate asks who
  issued it and what it is for. Nothing reads a subject's common name to decide anything. A TLS
  client checks a server certificate against its subject alternative names (RFC 9525 section 2).
  The Server tells an enrolment connection from a member's by its issuer, names a host by the SAN
  URI it puts in, and decides a renewal by its proof
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md),
  [CLT-modes-and-gateway](CLT-modes-and-gateway.md)). The subject is free to say what an auditor
  needs. The name an operator wants there is that of their own installation, which no build
  constant knows.
- **The operator's own machine comes first.** It is where an operator works before any Server
  exists. Whoever holds the package signing key and can make an offer runs code on every host it
  reaches. That is why the Client installs only what a key the operator holds has signed
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)), and why the server
  CA's key stays off the Server host. A program that makes those keys is a program for the
  operator's machine, and the binary that serves the fleet has no use for it.
- **One audience, one job.** The certificates, the signing key, the packing and the fetch are each
  done once per fleet or once per release, by the same person, on the same machine. A program per
  task, or a Server subcommand tree for part of it, is names too many.
- **Keys need file modes.** Linux and macOS give `0600` and `0700` by `chmod`. Windows has no such
  modes, and an ACL that does the same is a piece of work of its own.
- **Names are a public contract, and the obvious one is taken.** `fleetctl` is the command-line
  tool of FleetDM, an actively maintained fleet-management product for the same audience,
  installed with `npm install -g fleetctl` and from its GitHub releases. Two `fleetctl` on one
  `PATH` cannot both be found, and a search for either finds the other.
- **The release is a contract too.** It is one workflow run from one version
  ([OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md)). Every row
  of its build matrix already builds the packer for the runner it runs on, Windows included,
  because the packer packs the Client's archives.
- **Some places want a PEM pair while only a `bootstrap.p12` is at hand:** a host whose install is
  fed the pair, a tool that reads PEM only, an operator who wants to see what the file holds. The
  Client's install reads the file with a bounded reader in `fleet-util`, on the PKCS#12 code of
  `fleet-core`
  ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)). The
  operator program depends on both crates already.
- **Some upstreams ship no artifact a Client can run as shipped.** The operator program unpacks
  such a release on the operator's host and repacks the tree before it is signed. A tree package
  carries no link but those its kind keeps
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)), so the repack
  dereferences links. A linked directory is packed under the linked name with its contents,
  because that name is one the agent uses: a tree may reach its Perl library through
  `usr/share/perl/5.26`, a link to `5.26.1`. Resolving a link with `std::fs::metadata` and reading
  it with `File::open` follows an absolute target, or a `../..` target, out of the tree into the
  operator host's own file system. Such a link would do three kinds of harm:
  - it packs the operator's files into an artifact the operator signs and the fleet installs, a
    disclosure a compromised upstream can choose while its download still matches its checksum;
  - it makes the bytes depend on the operator's host, so the repack is not deterministic;
  - it names a path of the host upstream built for, never of the tree, so it means nothing.

  **Q-1** puts security before convenience.
- **A repacked tree carries upstream's modes unless the repack sets them.** A write bit for the
  group or for others, or a setuid bit, would travel into the signed artifact, with only the
  Client's mask on unpacking to take it off.
- **Some upstream trees need an absolute link.** An AppImage's runtime reaches the program loader
  of the host it runs on through one.
- **One upstream version can need a second repack,** one that takes newer files from its sources
  or is packed by a changed rule. The Server cannot hold it under the version of the first.
- **More than one kind's fetch verifies an upstream's OpenPGP signature.**
- **The reading of a certificate's end has a natural home in `fleet-core`.** `fleet-core` carries
  `x509-parser` and `time` for `x509` and `csr`. Each program still reads the file itself, through
  `opamp::tls`, the one reader of a certificate's PEM
  ([DEV-six-crates-tokio-axum §17](DEV-six-crates-tokio-axum.md)). The core `ending` stays plain
  date arithmetic without a dependency, as that clause describes it; the reading is its adapter
  `ending::read`.

This serves **G-17** (the connection is secured and the Agent is identified), **G-10**, and
**Q-1** (secure by default).

## Decision

We will build the operator tools as one program, `opamp-fleetctl`, shipped with every release for
Linux and macOS. The program does the following, with `rcgen` where it makes a key or a
certificate:

- it makes the fleet's certificate authorities, the Server's and a Gateway's certificate, and the
  package signing key;
- it names every certificate by its product and its role;
- it says, together with the Server, when a certificate is about to end;
- it turns a bootstrap PKCS#12 file into PEM;
- it repacks an upstream tree deterministically and from inside that tree alone.

No step of setting up or running a fleet needs `openssl` or another external tool. The one
exception is `gpgv`, for an upstream that signs with OpenPGP.

1. **The crate builds exactly this program.**
   - `crates/fleet-tools` has no library and one binary, `opamp-fleetctl`.
   - It keeps the dependency direction of
     [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md): it uses the items of the internal
     crates `fleet-core` and `fleet-util`, and it depends on neither end. Nothing depends on it.
   - The program is `src/main.rs`, which parses the command line. Each command tree has one
     module, `src/package.rs` and `src/pki.rs`. The release fetcher behind `package fetch` is
     `src/fetch.rs`.

2. **The release run builds it from the release's version, for four targets.**
   - The targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
     `aarch64-apple-darwin` and `x86_64-apple-darwin`.
   - Each is built in the row of the Client's build matrix for the same target.
   - Each row whose target runs on its runner asserts that `opamp-fleetctl --version` reports the
     release version, as the Client's build does.
   - The Windows row builds the program for its runner only as the packer of the Client's archive.
     The release carries no Windows archive of it.

3. **One archive per target, named like the Client's.**
   - The name is `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, by the name grammar and the
     platform tokens of
     [OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md).
   - `opamp-fleetctl package pack --format tar.gz` packs it, with the one member `opamp-fleetctl`
     and its executable mode.
   - No `.deb`, `.rpm` or `.msi` carries it.
   - The four archives are files of the release beside the Client's. The same `SHA256SUMS` covers
     them.

4. **It is not a package of the fleet.**
   - The release notes list the four archives as the operator tool. They say the operator
     downloads them and does not upload them to the Server.
   - Everything that selects the Client's archives must select them by their `supervisor_` stem,
     never by version and platform alone. That covers the notes' upload loop and
     `opamp-fleetctl package fetch --agent supervisor`. Otherwise the loop would put an
     `opamp-fleetctl` archive into the `supervisor` Package as the Entry of its platform.
   - An upload made by hand anyway installs nothing. The Client's self-update refuses an artifact
     whose member is not `supervisor`
     ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)).

5. **No dependency beyond the issuance crates, the PKCS#12 reader the Client shares, and
   `cryptoki` for a token.**
   - The `pki` commands use `rcgen` with the features
     [SEC-bootstrap-certificates-and-enrolment §13](SEC-bootstrap-certificates-and-enrolment.md)
     states for issuance, and `time`.
   - `p12-extract` also reads through `fleet_util::identity::pkcs12::open`, the reader the
     Client's install uses. It so reads through `fleet_core::pkcs12::file` and `p12-keystore`
     ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)).
   - `pki token-init` and `pki bootstrap-ca --pkcs11-module` reach a PKCS#11 token through
     `cryptoki`. The Server uses the same wrapper for the same token
     ([SEC-admission-by-client-certificate §35](SEC-admission-by-client-certificate.md)).
   - Every key is ECDSA P-256.
   - A key written to a file is PKCS#8 PEM, the form `[client_ca]` loads. A key made on a token
     stays on it.
   - Every certificate is PEM.
   - Every serial is 16 random bytes, as the client CA's serials are.

6. **Four commands make, and a status command runs where the files are.**
   - `opamp-fleetctl pki init` makes a fleet's certificates and its package signing key from
     nothing.
   - `opamp-fleetctl pki token-init` makes the client CA, the bootstrap CA and the audit key on a
     PKCS#11 token, on the Server host.
   - `opamp-fleetctl pki server-cert` makes a server certificate from the existing server CA, for
     the Server or for a Gateway.
   - `opamp-fleetctl pki bootstrap-ca` makes a new bootstrap CA to replace the Server's.
   - `opamp-fleetctl pki status` says when the certificates in the offline directory end.
     `server pki status` says when those of the Server host end.
   - The Server's command runs and exits before the Server starts serving, like
     `hash-credential` and `audit-verify`.
   - The Server binary carries no code that makes a CA or a server certificate. It signs host
     certificates ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md))
     and bootstrap certificates
     ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)).
   - When a certificate counts as ending (clause 21) is one rule in `fleet-core`, plain date
     arithmetic.
   - Reading a certificate's end, with the days it has left and the line `pki status` prints for
     it, is one function in `fleet-core`. It reads the first certificate each program reads from
     the file through `opamp::tls`. Both commands and the running Server so read and judge alike.
   - None of the commands makes a host certificate or a bootstrap certificate.

7. **`init` writes two directories, and the split is the point.**
   - `--server-dir` receives what the Server host needs: `server.pem` and `server-key.pem`,
     `client-ca.pem`, `bootstrap-ca.pem`, and `server.toml.fragment`. The fragment names them in
     `[tls]`, `[client_ca]` and `[enrolment]`. Beside them stand `client-ca-key.pem` and
     `bootstrap-ca-key.pem`, named in `key_file` and `bootstrap_ca_key_file`.
   - With `--ca-keys-on-token`, `init` makes neither the client CA nor the bootstrap CA and writes
     none of their four files. Its fragment then names `[tls]` alone. `pki token-init` makes both
     CAs on the Server host (clause 8).
   - `--offline-dir` receives what stays with the operator:
     - `server-ca.pem` and `server-ca-key.pem`;
     - the package signing key as `package-signing-key.pk8`, an Ed25519 key in PKCS#8 as
       `package keygen` writes it, and its public half in hex as `package-signing-key.pub`
       ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md));
     - `supervisor.toml.fragment`. It names `server-ca.pem` in `[tls] ca_file`, and the public
       half in `[packages] verification_key`. It names `bootstrap.pem` and `bootstrap-key.pem`
       under `--host-path` in `cert_file` and `key_file`, where a host puts the pair the Server
       issues it.
   - A command of the tool must never write a key of the server CA or a package signing key to
     `--server-dir` or to a token.
   - Each fragment names its files by the directory they are meant to live in. That is
     `--server-path`, by default the absolute path of `--server-dir`. It is `--host-path`, by
     default the absolute path of `--offline-dir`.
   - The command prints which directory goes where.

8. **`init` runs where the offline keys are to stay, and `token-init` where the token is.**
   - `init` runs on the operator's Linux or macOS machine, the one `opamp-fleetctl` is downloaded
     to, with or without `--ca-keys-on-token`.
   - Where `init` runs on the Server host, the operator moves `--offline-dir` off it afterwards.
     The command says so when both directories are on one file system.
   - A token the Server uses serves the Server's host, so the keys on it are made there. The
     command is `opamp-fleetctl pki token-init --pkcs11-module <path> --token-label <label>
     --pin-file <file> --out <dir>`, run on the Server host.
   - It generates the client CA's key, the bootstrap CA's key and the audit key on the token of
     [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md), with
     `CKA_EXTRACTABLE` false and `CKA_SENSITIVE` true.
   - It signs each CA certificate through the token with that CA's own key.
   - It writes `client-ca.pem`, `bootstrap-ca.pem` and `token.toml.fragment` to `--out`. The
     fragment names `[pkcs11]`, `[client_ca]` with `key_label`, `[enrolment]` with
     `bootstrap_ca_key_label`, and `[audit] key_label`.
   - It takes the flags clauses 13, 17 and 18 name for it.
   - It writes no key to a file, and it makes no server CA and no key that stays offline.
   - It refuses a label the token already holds, naming the label.

9. **The CAs are told apart by role and sign no CA.**
   - The three CAs are `CN=<product> Server CA`, `CN=<product> Client CA` and
     `CN=<product> Bootstrap CA` (clause 17).
   - The bootstrap CA therefore never shares a subject with the client CA
     ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)).
   - Each CA certificate is `CA:TRUE` with a path length of 0, `keyCertSign` and `cRLSign`.

10. **A server certificate names what is dialled in its SAN and its role in its subject.**
    - The operator gives `--name` at least once. It takes a DNS name or an IP address.
    - The command refuses an empty value, a wildcard, and a name that is neither.
    - Each name becomes a subject alternative name.
    - The command always adds `IP:127.0.0.1` and `IP:::1`. The Operator plane on its loopback
      default is so reachable through a tunnel with the same certificate.
    - The certificate is `CA:FALSE`, `digitalSignature`, `serverAuth`.
    - Its common name is derived from the server CA's (clause 19).
    - `pki server-cert` takes O and OU from the server CA. `--organization` and
      `--organizational-unit` replace either, so a Gateway run by another unit says so.

11. **`server-cert` serves the Server and a Gateway alike.**
    - `opamp-fleetctl pki server-cert --offline-dir <dir> --name … --out <dir>` signs
      `server.pem` and `server-key.pem` in `--out` with `server-ca-key.pem`.
    - For the Server, they replace the files of `[tls]`, and the operator restarts the Server on
      them.
    - For a Gateway, they are its `[gateway.tls]` `cert_file` and `key_file`
      ([CLT-modes-and-gateway](CLT-modes-and-gateway.md)). The Agents behind it verify them against
      the same `server-ca.pem`.
    - The fleet's trust does not change, so no host's configuration does.

12. **A new bootstrap CA replaces the Server's.**
    - `opamp-fleetctl pki bootstrap-ca --out <dir>` makes a new bootstrap CA, with the subject a
      bootstrap CA carries (clause 9).
    - Without `--pkcs11-module`, it writes `bootstrap-ca.pem` and `bootstrap-ca-key.pem` to
      `<dir>`. The operator copies both to the Server host in place of `[enrolment]`'s files.
    - With `--pkcs11-module`, `--token-label` and `--pin-file`, the command runs on the Server host.
      It generates the key on the token of
      [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md) under a new
      label. It writes `bootstrap-ca.pem` to `<dir>` and prints the label. The operator names that
      label in `bootstrap_ca_key_label`.
    - The command never overwrites a file or a token object.
    - The operator restarts the Server.
    - Bootstrap certificates from the old CA stop passing the handshake. The operator has those
      not yet used issued again.

13. **Lifetimes have defaults, each may be overridden, and none runs past the CA.**
    - The server CA and the client CA live 3650 days, the bootstrap CA 365 days, and a server
      certificate 397 days.
    - `init` takes `--ca-days` for the server CA and the client CA, `--bootstrap-ca-days`, and
      `--server-days`.
    - `token-init` takes `--ca-days` for the client CA, and `--bootstrap-ca-days`.
    - `server-cert` and `bootstrap-ca` take `--days` for the one certificate they make.
    - A server certificate ends no later than the server CA that signs it. The command cuts one
      asked to outlive the CA to the CA's end, with a notice.
    - Bootstrap certificates live as clause 14 states.
    - Host certificates are the running Server's. It signs them for `[client_ca] validity_days`, as
      [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md) states.

14. **A bootstrap certificate lives 10 days by default and at most 90, never past its CA.**
    - `validity_days` of the issuing request runs from 1 to 90. The Server answers anything else
      with `400`.
    - A certificate that would outlive the bootstrap CA ends with it, and the response's
      `not_after` says so.

15. **Nothing is overwritten, and nothing is half written.**
    - A command that would write a file that exists refuses before it writes anything, naming the
      file.
    - It writes into a new temporary directory beside its target. It renames into place only once
      every file is complete.
    - Directories it creates are `0700`, and every key is `0600`.

16. **Key files are stored unencrypted, and token keys never leave the token.**
    - Where the Server reads a CA key from a file, it reads it unattended.
    - Where they are kept and their mode protect that file and the offline keys, as they protect
      every other key of the project.
    - A key on the token of
      [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md) is created
      non-extractable. The Server uses it only through `C_Sign` and never reads it.
    - The secrets left on the Server's host are the PIN file and the TLS listener's key. With
      SoftHSM2, its token directory is one too: with the PIN, it is a copy of every key on the
      token.

17. **The product is a flag, with a default.**
    - `opamp-fleetctl pki init`, `pki token-init` and `pki bootstrap-ca` take `--product <name>`,
      by default `OpAMP Fleet`.
    - The name holds letters, digits, space, `.` and `-`, 1 to 40 characters, with no leading or
      trailing space.
    - No build constant enters a certificate.

18. **The operator names an organization for audit.**
    - `init`, `token-init` and `bootstrap-ca` take `--organization` (O) and
      `--organizational-unit` (OU). Each is optional.
    - Each is 1 to 64 characters of UTF-8 without control characters, the upper bound of RFC 5280
      Appendix A.1.
    - `init` writes them into every CA it makes and into the server certificate. `token-init`
      writes them into the two CAs it makes.
    - The commands encode attributes as `UTF8String`, in the order O, OU, CN.
    - They write no other attribute.

19. **A leaf's common name is its CA's, with `CA` turned into `Certificate`.**
    - Whoever signs a leaf takes the common name of the CA that signs it. `opamp-fleetctl` signs a
      server certificate; the Server signs a host or a bootstrap certificate.
    - The signer replaces a trailing ` CA` with ` Certificate`, or appends ` Certificate` when
      there is none.
    - A fleet made by `init` therefore has `<product> Server Certificate`,
      `<product> Client Certificate` and `<product> Bootstrap Certificate`.
    - A fleet whose CAs were made elsewhere gets a name from them without a setting of its own.
    - The Server refuses to start when the name derived from the client CA or the bootstrap CA
      would exceed 64 characters.

20. **A development certificate says so.**
    - `scripts/dev-pki.sh` makes its set with `--product "OpAMP Fleet Dev"
      --organization "Development"`.
    - `scripts/seed_test_configs.sh --enrol` issues its bootstrap certificate with the
      organization `Development`.
    - Every certificate of a development set therefore reads `OpAMP Fleet Dev …` and
      `O=Development`. One that turns up outside a developer's machine is recognized as such.

21. **The running Server warns before a certificate it depends on ends.**
    - At startup and once a day while it runs, the Server reads the end of these certificates:
      - its own certificate (`[tls] cert_file`);
      - the client CA (`[tls] client_ca_file` and `[client_ca] cert_file`);
      - the bootstrap CA (`[enrolment] bootstrap_ca_file`).
    - A certificate is *ending* when it ends within 30 days. A certificate whose whole life is
      shorter than 90 days is *ending* within the last third of that life.
    - The Server logs an ending certificate as a warning naming the file, its subject and the
      date. It records it in the audit as `pki.expiring`.
    - The Server logs a certificate that has ended as an error. It records it as `pki.expired`
      ([SEC-append-only-audit-record](SEC-append-only-audit-record.md)).
    - Neither stops the Server. The hosts it serves decide for themselves, and nobody can tell a
      Server that refuses to start to renew.

22. **`pki status` says when each certificate ends.**
    - `server pki status --config <server.toml>` lists the files of clause 21.
    - `opamp-fleetctl pki status --offline-dir <dir>` lists the server CA.
    - Each prints every certificate with its subject, its serial, its end and the days left.
    - Each exits `0` when no certificate is ending in the sense of clause 21, `1` when one is, and
      `2` when one has ended. A monitoring job may so run it.

23. **The project uses its own commands.**
    - `scripts/dev-pki.sh` makes its development set with `cargo run -p fleet-tools -- pki init`.
      It writes a complete `server.toml` and `supervisor.toml` from the fragments and needs no
      `openssl`.
    - Its development Client holds a bootstrap pair the development Server issued.
      `scripts/seed_test_configs.sh --enrol` asks the Server for the pair and writes it where
      `supervisor.toml` names it. It then opens the window and approves the request.
    - The operator manual describes the `pki` commands and the issuing route. It reads a serial
      with `pki status` or from the route's answer, and it names no external tool.
    - Certificates made elsewhere keep working. The Server reads any PEM that meets the
      requirements these commands write down.

24. **`p12-extract` reads what the Client's install reads.**
    - `opamp-fleetctl pki p12-extract <file> --out <dir>` opens `<file>` with
      `fleet_util::identity::pkcs12::open`, the reader the Client's install uses.
    - It therefore takes exactly the bootstrap PKCS#12 files that
      [SEC-bootstrap-certificates-and-enrolment §4 and §15](SEC-bootstrap-certificates-and-enrolment.md)
      describe, by the file's form and by how the Client reads it.
    - It refuses the rest, naming why. That covers these:
      - a file over the reader's size bound;
      - a wrong password;
      - a scheme other than PBES2/AES with an HMAC-SHA-2 MAC;
      - anything other than one key with its one certificate.

25. **Where the password comes from.**
    - The command takes the password from a hidden question on a terminal, or from
      `--password-file <path>`. That file holds the password on its first line.
    - It must never take the password from a command-line value or an environment variable. Other
      users or the shell history see either.
    - Without a terminal and without `--password-file`, the command fails before it reads the file.

26. **What `p12-extract` writes.**
    - It writes `<dir>/bootstrap.pem`, the certificate, and `<dir>/bootstrap-key.pem`, the key as
      unencrypted PKCS#8. These are the names of the pair the Client's install finds
      ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)).
    - On Unix it writes the key `0600`.
    - It creates `<dir>` when it is missing.
    - It overwrites nothing (clause 15). If either file exists, it writes neither and names the one
      in the way.
    - It prints the certificate's subject, serial and `notAfter`. It never prints the key or the
      password.

27. **Every repack that `opamp-fleetctl package fetch` makes is deterministic, and it carries
    nothing from outside its tree.**
    - Some upstreams ship no artifact a Client can run as shipped. The tool repacks such a release
      into a `.tar.gz` on the operator's host, before it is signed.
    - The repack stores normalized file modes. A directory, and a file with any execute bit set,
      is `0o755`. Every other file is `0o644`. No write bit for the group or for others, and no
      setuid, setgid or sticky bit, reaches the signed artifact.
    - The repack has a fixed member order and zeroed times and ownership.
    - It carries no link but those its kind keeps.
    - **A link is resolved inside the tree.** Its target is taken relative to the directory holding
      the link. Every link met on the way is resolved by the same rule. A link that resolves to a
      file or a directory inside the tree is dereferenced. A linked directory is packed under the
      linked name too, with its contents.
    - **A link that leads out of the tree fails the repack.** A link leads out when its target is
      absolute, or when resolving it climbs above the root of the tree. That holds whether or not
      anything exists at the path it would name on the operator's host. The repack stops before it
      writes the artifact. It names every such link by its path in the tree and its target. It
      reads nothing of the operator's host through the link.
    - **A kind may keep an absolute link, which is packed as that link.**
      - The kind names the link by its exact path in the tree and its exact absolute target.
      - The repack keeps only a link at that path whose target is byte for byte that one. It must
        meet the link at that path, not through a linked directory.
      - It packs that link with mode `0o777`, reads it from the tree and never follows it. So
        nothing of the operator's host is read through it.
      - The Client creates exactly those links
        ([PKG-signed-packages-and-delivery §57](PKG-signed-packages-and-delivery.md)).
      - A link at a named path with another target leads out and fails the repack, as the bullet
        above states. So does every link the kind does not name.
      - The shared rule is `fleet_util::tree::is_kept`.
    - **A link that names nothing inside the tree is dropped.**
    - **A link leading back up the tree is not followed,** the root of the tree included. A link to
      a directory the walk is already inside is packed as that name and not descended into.
    - The repack checks the member cap a tree package has
      ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)) while it packs.

    Repacking the same release therefore yields the same bytes and the same hash on any
    operator's host, and no accidental rollout.

28. **A later repack of one upstream version is a pre-release of the next patch.**
    - The first repack of a version is the Package that version names
      ([PKG-versions-high-water-mark-floor §6](PKG-versions-high-water-mark-floor.md)).
    - A later repack of the same version takes newer files from its sources or is packed by a
      changed rule. The operator names it by `opamp-fleetctl package fetch --repack <n>`, with `n`
      from 2.
    - It is uploaded as Package `<major>.<minor>.<patch+1>-0.r.<n>`. The second repack of `2.14.3`
      is `2.14.4-0.r.2`.
    - **It orders between the version and the next release.**
      - In SemVer precedence, it lies above the first repack and every earlier one, and below
        `<major>.<minor>.<patch+1>`.
      - The repack number is a numeric identifier of its own, so it is compared as a number.
        `r.9` orders below `r.10`, up to the largest number the flag takes.
      - The leading `0` orders every repack below any pre-release upstream names for the next
        patch.
    - **The program keeps the version it is.**
      - The repacked program keeps reporting the upstream version.
      - The Server holds the Package against the version of the Package the host's Supervisor
        installed
        ([SRV-deployments-and-pinned-rollouts §14 and §25](SRV-deployments-and-pinned-rollouts.md)).
      - A host on an earlier repack is offered the later one. A host on the later one is offered it
        no more. Upstream's next release still replaces it.
      - Build metadata (`2.14.3+r2`) is not the form. Two versions that differ only in it compare
        equal ([PKG-versions-high-water-mark-floor §2](PKG-versions-high-water-mark-floor.md)).
    - **Refused:**
      - `--repack 0`;
      - a later repack of a version that is not three numbers;
      - a later repack of an agent whose artifact the tool uploads as upstream ships it, which has
        one artifact per version.
    - `--repack 1`, the default, is the version itself.
    - The operator counts on from the last repack uploaded. The tool does not look it up.

29. **An upstream's OpenPGP signature is verified with `gpgv`, against a key the tool carries, by
    a pinned primary key.**
    - Where a kind's upstream signs what `package fetch` downloads with OpenPGP, the tool verifies
      it with `gpgv`. The signature is a detached one beside an artifact or a clear-signed
      repository index.
    - The tool verifies against a binary keyring it carries for that upstream, never a keyring of
      the host.
    - **A home of its own.**
      - The tool writes the keyring and the signature to a directory of the process's own, under
        the system's temporary directory, with mode `0700`: no other user may read or write it. It
        is `gpgv`'s home.
      - The tool names every file to `gpgv` by its absolute path, so `gpgv` reads no keyring and no
        option of the user's.
      - The tool removes the directory after the run.
    - **The verdict is `gpgv`'s status lines, never its prose.**
      - It needs a `VALIDSIG` whose last field is one of the primary fingerprints the kind pins for
        that upstream. That field names the primary key of the key that made the signature.
      - It needs no `BADSIG`.
      - A signature by a subkey is its primary's. A subkey that has expired since it signed still
        counts.
    - **A detached signature also needs a run that `gpgv` calls a success.** The file beside an
      artifact carries the one signature its upstream made. A non-zero exit status refuses it,
      whatever the status lines say.
    - **A clear-signed text is judged by the status lines alone.** An archive signs its index with
      several keys at once. A signature by a key the keyring does not hold (`ERRSIG`) decides
      nothing either way and makes the exit status non-zero. So the tool does not read the exit
      status there.
    - **A clear-signed text is the text the signature covers.** The tool reads what `gpgv` writes
      out as signed. It never reads the file as it came, which may carry more around the signed
      part.
    - **No `gpgv`, no fetch.** A fetch that has to verify a signature and finds no `gpgv` fails,
      naming it. The tool installs nothing.

    Each kind states which upstream signs, with which key, from which version on, and whether a
    missing signature refuses the artifact.

**Out of scope:**

- the names of the command trees of `opamp-fleetctl`, which commands each holds, and the absence
  of any other program name for them, which
  [PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md) states;
- issuing a host certificate by any route other than enrolment, which the specification rules out;
- how the Server issues, records, hands out and revokes bootstrap certificates, and what of their
  subject a host certificate carries
  ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md));
- customers as a resource of the Server;
- attributes other than O, OU and CN in what `pki init`, `pki token-init`, `pki server-cert` and
  `pki bootstrap-ca` write;
- encrypting key files at rest, or a passphrase prompt;
- a token for the server CA's key or the package signing key, which stay offline with the
  operator;
- replacing the server CA, which every host's configuration follows;
- replacing the client CA, which every host's enrolment follows, except for the move of a fleet
  onto a token. `pki token-init` makes its new client CA, and
  [SEC-admission-by-client-certificate §40](SEC-admission-by-client-certificate.md) rolls it out;
- holding two bootstrap CAs at once while one replaces the other;
- enforcing a CA's end in the handshake, or limiting the running Server's signatures to its CA's
  end;
- certificate revocation lists or OCSP;
- ACME and certificates from a public CA, which the Server reads as they are;
- a `pki` command on the Client binary;
- the Server's `pki status` on Windows or macOS, where the Server does not run;
- the operator tool on Windows;
- native packages, Homebrew, `npm` or any other installer;
- provenance of the release's files
  ([OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md));
- commands that drive the running fleet through the Operator plane, beyond the upload
  `package fetch` makes;
- writing the extracted key encrypted, extracting files other than the bootstrap form, and making
  a PKCS#12 file with the tool;
- how the Client applies a tar's modes, which
  [CLT-accounts-service-runs-under §23](CLT-accounts-service-runs-under.md) states;
- a kind's own removal, before packing, of links it knows upstream leaves behind, as an AppImage's
  repack removes `.DirIcon`. Each kind states that where it is decided;
- hard links, which are files of the tree;
- the trust in the extracted tree's origin, which the hash of what was downloaded settles;
- following an upstream's key at run time; a new key reaches the tool in a new carried keyring
  with a release of the tool;
- a revocation the carried keyring does not state, which `gpgv` cannot know of.

## Alternatives considered

- **Keep `openssl` and document it better.** Rejected: the rules stay in prose an operator has to
  get right by hand, and a system tool whose behaviour differs between versions stays a
  prerequisite of every fleet. OpenSSL 1.1 cannot open a `bootstrap.p12`, and with OpenSSL 3 the
  operator has to know the options.
- **Recommend `step` or `cfssl`.** Either makes certificates well, but each is one more tool to
  install and to learn, and neither knows this project's requirements.
- **All the commands on the Server binary,** beside `pki status`, `hash-credential` and
  `audit-verify`. The Server binary is certain to be at hand. But it is the binary of the host the
  package signing key and the server CA's key are kept away from. The archive writers and the
  release fetcher would be linked into the binary that faces the fleet: attack surface with no use
  in serving, since the Server never creates or opens an artifact
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)). With the operator
  tool in the release, its one argument, no second binary to obtain, is gone; both share the
  release's version.
- **Put the commands into the Client binary.** The Client is in the release already, but it lands
  on every host, and every host would then carry the means to make a CA and a signing key.
- **Two programs, one for packages and one for certificates, both in the release.** It lifts the
  toolchain requirement, but leaves two names for one person's one job.
- **Keep the tools out of the release.** Every operator builds them from a checkout, and a fleet
  cannot install a single signed package without a Rust toolchain on someone's machine.
- **Name it `fleetctl`.** It draws the `kubectl` analogy the crate split already draws, but the
  name belongs to FleetDM's tool (Context).
- **Name it `opamp-operator` or `opamp-fleet`.** The first says who uses it, but a name ending in
  `-ctl` says it is the command-line tool of a system, which is what an operator looks for. The
  second is the build-time product name the Client's service is registered under
  ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)), so a program of that
  name reads as the Client.
- **All five targets, Windows included.** Windows would need its own way to keep a key readable by
  its user alone, a piece of work this decision does not take on.
- **Linux only.** Simpler by two builds, but an operator on macOS, a likely operator machine,
  would again need a toolchain, and macOS gives the same file modes as Linux.
- **Keep the bootstrap CA offline and make bootstrap certificates with `opamp-fleetctl`.** It gives
  each customer a subject of its own. But each certificate is a trip to the offline machine, and
  the Server neither lists nor knows what was issued until a host presents it. It protects a key
  whose loss the client CA's key on the Server already outweighs.
- **One CA for client and bootstrap certificates, with the bootstrap ones marked by an extension
  or found in a register.** It saves one key pair, but it makes admission depend on a second check
  that lets a host in when it fails. With a CA of its own, a failure falls to the safe side: an
  enrolment connection.
- **One CA for everything.** It combines the above with the server CA's key online. A copy of the
  Server's key would then impersonate the Server to every host.
- **Issue a host certificate from the command line,** offline with the client CA's key or through
  a route of the running Server. Rejected: the specification has a host obtain its certificate
  only through an approved, time-limited enrolment over the protocol's own means, never through a
  side channel. A route would also let the operator's credential alone mint a member.
- **Every CA key the Server uses in a file, a hardware security module out of scope.** No token,
  no `cryptoki`, one way to set up. Rejected: a key file the Server reads unattended can be copied
  from the host, a backup or a snapshot, and a copy signs member certificates nobody can tell from
  real ones. Several Server instances would each need a copy. A file stays one of the two places
  (clauses 7, 8 and 16); the token is the other.
- **Encrypt the offline CA keys with a passphrase.** It needs an encryption crate beyond clause 5
  and a prompt the commands must handle. Kept out of scope, not ruled out.
- **Shorter lifetimes for the server CA and the client CA too.** The online client CA's key would
  be replaced more often. But replacing it means every host enrols again, and replacing the server
  CA means a new `ca_file` on every host. Without a way to hold two CAs at once, both stay at ten
  years.
- **Leave the package signing key to `package keygen` alone.** It keeps `init` to certificates,
  but a fleet that skips the second step has no key, and its Clients take no package until someone
  sets `verification_key` by hand. The key belongs where `init` already puts what stays offline,
  and `keygen` remains for a second key or a replacement.
- **Country, state and locality in the subjects `pki init` writes as well.** They describe where an
  organization sits, which an audit of the CAs and the server certificate does not ask. `country`
  also needs an encoding of its own (`PrintableString`). O and OU name who is responsible.
- **The product name from the build (`PRODUCT_TITLE`).** It needs no flag. But it names the Client
  product (`OpAMP Fleet Agent`), lives only in `crates/fleet-agent`, and is the same for every
  installation of one build. An operator's certificates name their installation.
- **A slug for the fleet (`--fleet opamp-fleet`) and the role in lower case.** The smallest
  naming, but an auditor reads a slug and no organization, and no leaf shows its role in its
  subject.
- **A configuration file for issuer and subject data,** or a setting for the leaf names in
  `server.toml`. Either holds everything in one place. But three values are set at most once a
  year, a file has to be versioned, validated and documented, and leaf names set apart from the
  CA's can drift from it. An operator with richer requirements makes the certificates in their own
  PKI, which the Server accepts (clause 23).
- **Each program reads the certificate itself, with only the date rule in `fleet-core`.** It would
  keep the internal crate free of `x509-parser`, `time` and a PEM reader. But `fleet-core` carries
  those dependencies for `x509` and `csr` anyway, and the reading and the line `pki status` prints
  would be written twice, verbatim, in `fleet-server` and `fleet-tools`. Rejected for clause 6's
  one function.
- **`fleet-tools` depending on `fleet-server` for the rule.** No new code in `fleet-core`, but the
  operator tool would link the Server's library, its router included, for a dozen lines.
- **`pki status` in one program only.** The Server host does not hold the server CA's certificate,
  and the operator's machine does not hold `server.toml`. Either program alone sees only part of
  the certificates, and the server CA's end, which every host's trust hangs on, would be visible
  nowhere.
- **Limit every signature of the running Server to its CA's end, and refuse renewals under an ended
  CA.** It belongs with how the Server signs
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)); this decision
  warns instead (clauses 21 and 22).
- **`p12-extract` opens the file with the Client's own reader, depending on `fleet-agent`.**
  Rejected: it links the whole Client into the operator program
  ([DEV-six-crates-tokio-axum §22](DEV-six-crates-tokio-axum.md)). The same reader lives in
  `fleet-util`, which both use.
- **`p12-extract` writes the key encrypted (PKCS#8 `ENCRYPTED PRIVATE KEY`).** The key stays
  protected at rest. But the Client's install reads the pair in the clear
  ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)), and neither `rustls`
  nor `rcgen` reads an encrypted key. Permissions for the file's user alone are what every other
  key is given ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)).
- **`p12-extract` writes the certificate only.** The key never leaves the file. Not chosen: the
  operator asked for the key, and the PEM pair is a form the Client takes.
- **Read any PKCS#12 file (other schemes, several certificates).** More general, but it needs a
  second reader with weaker schemes. The bootstrap file is the only PKCS#12 file this project
  makes.
- **A command of its own outside `pki` (for example `opamp-fleetctl p12 extract`).** A third tree
  for one command. The certificates already live under `pki`.
- **The password as `--password <value>`.** It would be visible in `ps` and in the shell history.
- **The repack follows every link, wherever it leads.** Simple, and close to `tar --dereference`.
  Rejected: it ships the operator's files to the fleet under a signature, and it makes the bytes
  depend on the operator's host, which breaks the repack's own promise of determinism.
- **Drop a link that leads out of the tree, and report it.** Nothing leaks either, the outcome is
  host-independent if the test is the same one, and a tree with stray links still repacks.
  Rejected as the default: the artifact then silently differs from the tree upstream built. A line
  on the operator's terminal, before a signature, is easily missed and never reaches the fleet. An
  agent that needed the path fails on every host it is rolled out to, rather than on the one host
  where it was packed. Failing puts the decision before the signature, where the operator is. A
  kind whose upstream reliably leaves such links removes them by name before packing, a reviewed,
  per-kind choice rather than a general silence. Dropping is cheaper to the operator; hardening
  outranks that.
- **Fail every link that leads out of the tree.** The strictest rule, and no tree package would
  carry a link the installing host resolves. Rejected: an AppImage's runtime reaches the host's
  program loader through an absolute link, so its kind could not be repacked. A link the kind
  names by its exact path and target is kept, and every other still fails (clause 27).
- **Pack every out-of-tree link as a link.** Faithful to upstream, but a link whose target nobody
  reviewed names whatever the installing host holds there, and the Client would have to create
  any link an archive carries. Rejected. Only a link the kind names by path and target is packed
  as one, a reviewed, per-kind choice the Client holds the archive against.
- **Keep the file modes upstream set.** Faithful to upstream, and the Client's mask on unpacking
  takes some bits off. Rejected: a group or world write bit, or a setuid bit, would travel into an
  artifact the operator signs, with the Client's mask as the only guard. The repack stores
  normalized modes (clause 27).
- **Decide by the canonical path on the operator's host** (`canonicalize` the target and compare it
  with the canonical root). Simple to write, but a target that does not exist cannot be
  canonicalized. A dangling absolute link would be dropped on one host and refused on another, and
  the result would again depend on the host. Rejected for the rule; it may still serve inside the
  implementation for targets inside the tree.
- **Confine the walk with the operating system** (`openat2` with `RESOLVE_BENEATH`, or a chroot).
  Strong where it exists, but Linux-only, and the repack also runs on macOS. Left to the
  implementation as a further guard, not the rule.
- **One repack per upstream version, under that version.** No numbering to learn. Rejected: a
  repack that takes newer files from its sources, or is packed by a changed rule, could not be
  uploaded, since the Server holds one Package per version (clause 28).
- **Number a later repack `-0.r<n>`, the number fused to the letter (`2.14.4-0.r2`).** One
  identifier fewer, but it is alphanumeric and compared as text. `r10` would order below `r9`, and
  the tenth repack would have to be refused. Rejected for the numeric identifier of clause 28.
- **Number a later repack by build metadata (`2.14.3+r2`).** It keeps the upstream version
  visible. But two versions that differ only in it compare equal, so the Server holds one of them
  and a host on the first is never offered the second. Rejected.
- **Leave an upstream's OpenPGP signature to each kind's fetch.** Each kind would verify in its own
  way. Rejected: more than one kind's upstream signs with OpenPGP, and one tested verdict verifies
  them alike (clause 29).
- **Verify OpenPGP with a Rust crate (`sequoia-openpgp`, `pgp`).** No program on the fetch host,
  but a large dependency with its own cryptography, for a check the operator runs a few times a
  release, against clause 5's short list. `gpgv` is the verifier `apt` itself uses, and the
  verdict is read from its machine-readable lines. Rejected.
- **Trust the host's keyrings (`/usr/share/keyrings`, `apt`'s trusted keys).** No keyring in the
  tool. But what the artifact carries would then depend on files of whichever host fetches it, and
  on hosts without them nothing verifies. Rejected for the carried keyring.

## Sources / Prior art

- HashiCorp Consul, `consul tls ca create`: a self-signed CA made by the product's own binary, with
  a days option and a default common name. <https://developer.hashicorp.com/consul/commands/tls/ca>
- HashiCorp Nomad, `nomad tls ca create` and `nomad tls cert create`: a CA and server, client and
  CLI certificates from the product's own binary.
  <https://developer.hashicorp.com/nomad/commands/tls/ca-create>,
  <https://developer.hashicorp.com/nomad/docs/commands/tls/cert-create>
- Nomad and Consul put their certificate commands into the one binary that also runs the servers
  (`nomad tls`, `consul tls`). This decision does not follow that arrangement, because the
  operator's offline keys stay off the server host.
- Kubernetes, `kubeadm init phase certs ca`: the cluster's CAs generated by the setup tool itself,
  skipping files that exist.
  <https://kubernetes.io/docs/reference/setup-tools/kubeadm/kubeadm-init-phase>
- Kubernetes TLS bootstrapping: a short-lived bootstrap token issued by the control plane, which
  approves the CSR it carries.
  <https://kubernetes.io/docs/reference/access-authn-authz/kubelet-tls-bootstrapping/>
- Kubernetes PKI: the CAs are named for their role (`kubernetes`, `etcd-ca`, `front-proxy-ca`),
  and the kubelet's client signer permits one subject only, `O=system:nodes`,
  `CN=system:node:<name>`, whatever the CSR asks for.
  <https://kubernetes.io/docs/setup/best-practices/certificates/>,
  <https://kubernetes.io/docs/reference/access-authn-authz/certificate-signing-requests/>
- Kubernetes offers `kubectl` per platform as a download of its own, apart from the components that
  run on the cluster's machines. <https://kubernetes.io/releases/download/>
- HashiCorp Vault's PKI engine: an issuing CA online, key pairs generated on request and returned
  once, a root kept offline. <https://developer.hashicorp.com/vault/docs/secrets/pki>
- `rcgen`: `CertificateParams::self_signed` and `signed_by`, `BasicConstraints::Constrained`.
  <https://docs.rs/rcgen>
- `rustls-pki-types` `TrustAnchor`: a subject, a key and name constraints, no validity. That is why
  an expired CA goes unnoticed by this project's own ends. <https://docs.rs/rustls-pki-types>
- RFC 5280 section 4.1.2.4, section 4.1.2.6 and Appendix A.1: the issuer and subject, the
  attribute types, and the upper bound of 64 characters for organization, organizational unit and
  common name. <https://www.rfc-editor.org/rfc/rfc5280>
- RFC 9525 section 2: the common name must not be used to identify a service.
  <https://www.rfc-editor.org/rfc/rfc9525>
- CA/Browser Forum Baseline Requirements, section 7.1.4: a publicly trusted server certificate's
  common name, when present, repeats one of its SANs. The rule binds public CAs only; the server CA
  here is private. <https://cabforum.org/working-groups/server/baseline-requirements/>
- FleetDM's `fleetctl`: installed with `npm install -g fleetctl`, from an install script, or from
  its GitHub releases. <https://fleetdm.com/guides/fleetctl>,
  <https://www.npmjs.com/package/fleetctl>
- `openssl pkcs12 -in bootstrap.p12 -nodes` (OpenSSL 3 man page `openssl-pkcs12`): the established
  way to turn a PKCS#12 file into PEM. It reads PBES2/AES files only from OpenSSL 3 on. Consulted
  from memory.
- rsync's `--safe-links` (ignore a link that points outside the copied tree) and
  `--copy-unsafe-links` (copy what it points to): two established answers to the same question,
  the first close to the dropping alternative. Recalled from the rsync manual, not re-read.
- GNU tar's `--dereference` (`-h`), which the repack follows for links inside the tree and which
  follows a link wherever it leads. Recalled, not re-read.
- CWE-59, *Improper Link Resolution Before File Access ('Link Following')*: the class an
  out-of-tree link belongs to. Recalled, not re-read.
- Reproducible Builds (reproducible-builds.org): an artifact's bytes must not depend on the build
  host's file system beyond its declared inputs. Recalled, not re-read.
- This project's own requirements:
  [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md) on admission, the
  client CA and how the Server signs;
  [SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md) on the
  bootstrap CA, the crates issuance adds, the `bootstrap.p12` file and its reader;
  [SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md) on the planes;
  [`docs/SPECIFICATION.md`](../SPECIFICATION.md) on how an Agent obtains its certificate and which
  private key the Server may make;
  [PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md) on why a package is
  signed by a key the operator holds and what a tree package may carry;
  [CLT-installed-service-and-state](CLT-installed-service-and-state.md) on the member a package
  artifact carries;
  [OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md) on the
  release, its naming and its files.

## Consequences

- Positive: a fleet is set up with two binaries the project builds and nothing else (G-17, Q-1).
  The requirements a hand-made certificate can miss are written once, in tested code. The offline
  keys have a place of their own from the first command on.
- Positive: an ending CA or server certificate is announced 30 days ahead in the log, in the audit,
  and by a command a monitor can run.
- Positive: an operator downloads one program and can make the fleet's certificates, a signing key
  and signed packages without a toolchain (G-10, G-17, Q-1). The two keys that stay on the
  operator's machine, the server CA's and the package signing key, each do what nothing on the
  Server host can. The Server binary carries no command that makes them. The packer the release
  uses is the one an operator downloads.
- Positive: the bootstrap CA is replaced every year at the cost of one restart and the unused
  bootstrap certificates.
- Positive: every certificate says what it is for in plain words, and each CA and server
  certificate names the organization behind it. An audit reads both from the certificate
  presented. A fleet with CAs from its own PKI gets leaf names that match them without a setting.
  A development certificate is told from a production one by its subject alone.
- Positive: an operator turns a `bootstrap.p12` into the PEM pair with the project's own tool, on
  Linux and macOS, without OpenSSL.
- Positive: no file of the operator's host reaches a signed artifact through a link. The bytes of
  a repack depend on the release alone, so a repack is deterministic across operators. The
  operator learns of an out-of-tree link before signing, by name, on the host where it can be
  judged.
- Positive: a later repack of a version reaches every host on an earlier one and gives way to
  upstream's next release by SemVer's order. It is held against the version of the Package a
  host's Supervisor installed, not the program's own. Two kinds that verify OpenPGP signatures
  verify them alike, by one tested verdict.
- Negative / trade-offs: the Server binary carries `pki status` beside serving, and the operator
  tool carries a second job beside packages.
- Negative / trade-offs: a fresh development set needs a running Server before its Client can
  start, and the development Client enrols, which adds an approval to a fresh development setup.
- Negative / trade-offs: keys at rest are only as safe as the place they are kept. A host
  certificate the running Server signs can still outlive its CA; the warnings announce that rather
  than prevent it.
- Negative / trade-offs: a server certificate's common name is not one of its names, so a public
  CA could not issue it in this form. It never does, the server CA being the fleet's own.
- Negative / trade-offs: four more builds and four more files per release; the release carries a
  program that makes keys; an operator on Windows still builds from a checkout.
- Negative / trade-offs: an extracted key lies in the clear on whatever disk `--out` names, as the
  PEM pair the Client's install takes does. Protecting that directory is the operator's job.
- Negative / trade-offs: an upstream tree with an absolute or escaping link does not repack until
  the kind removes that link by name or names it as a link it keeps. A kept link names a path on
  the installing host, which the kind's ADR answers for. Resolving a link component by component
  inside the tree is more code than calling `metadata`; the member cap and the ancestor guard
  against cycles bound it.
- Negative / trade-offs: a later repack's version names a patch upstream has not released, and the
  operator, not the tool, counts the repacks. A fetch of a kind whose upstream signs with OpenPGP
  needs `gpgv` on the host, and a new upstream key needs a release of the tool.
- Follow-ups: encrypting the offline keys; replacing a CA without every host enrolling again, and
  holding two bootstrap CAs while one replaces the other; enforcing a CA's end in the handshake, or
  limiting the running Server's signatures to it; showing the issuer's O and OU wherever the Server
  lists certificates; a view of the certificates' ends in the bundled UI; the operator tool on
  Windows with key files only their user can read; a command tree for the running fleet, if one is
  ever wanted, under this program's name; whether the Enrolments tab offers the PEM pair, which the
  tool can make; whether the Client's tree extraction clears group and world write bits regardless
  of how the artifact was built.

## Enforcement

Tests that carry `Verifies: OPS-operator-program-fleetctl`:

- `pki init` writes exactly the files of clause 7 into the two directories (clauses 7 and 15):
  - `init_splits_what_the_server_needs_from_what_stays_offline`: the bootstrap CA's key goes into
    `--server-dir`, no server-CA key and no package signing key go into it, and the fragments name
    `--server-path` and `--host-path`;
  - `init_makes_the_package_signing_key_offline_and_names_its_public_half`:
    `package-signing-key.pk8` and `package-signing-key.pub` go into `--offline-dir` alone, and the
    key signs a package the Client's verifier accepts under the `verification_key` the fragment
    names;
  - `keys_are_owner_only_and_new_directories_private` and
    `keygen_writes_the_key_owner_only_and_never_replaces_one`: keys are `0600`, directories
    `0700`;
  - `a_second_init_refuses_and_writes_nothing`: a second run over the same directories is refused
    without writing anything.
- `the_bootstrap_ca_lives_a_year_and_the_others_ten`: the bootstrap CA ends 365 days after it
  starts, the server CA and the client CA 3650. `a_certificate_never_outlives_its_ca`: a server
  certificate asked to outlive its CA ends with the CA (clause 13).
- `init_names_every_certificate_by_product_role_and_organization`,
  `the_cas_have_their_own_subjects_and_sign_no_ca`,
  `a_server_certificate_names_what_is_dialled_and_the_loopback`,
  `a_bad_product_or_organization_is_refused`,
  `a_wildcard_an_empty_name_and_a_bad_product_are_refused`, and in `fleet-core`
  `a_product_and_an_organization_keep_to_their_grammar` (clauses 9, 10, 17 and 18):
  - `pki init --product "Acme Fleet" --organization "Acme AG" --organizational-unit "IT"` writes
    CAs whose subjects are `O=Acme AG, OU=IT, CN=Acme Fleet Server CA`, `… Client CA` and
    `… Bootstrap CA`;
  - each CA has a path length of 0 and passes the Server's startup check;
  - the server certificate is `O=Acme AG, OU=IT, CN=Acme Fleet Server Certificate`, with every
    `--name` and the loopback as SANs;
  - without the flags, the CAs are `CN=OpAMP Fleet … CA` and carry no O or OU;
  - `--fleet` is an unknown option;
  - a product or an organization outside its grammar, a wildcard, and an empty `--name` are
    refused.
- `server_cert_signs_with_the_offline_server_ca` and
  `server_cert_takes_the_cas_organization_and_a_flag_replaces_it`: `pki server-cert` signs with the
  offline CA. A Client that trusts the old `server-ca.pem` accepts the new server certificate. The
  certificate copies O and OU from the server CA, and `--organizational-unit` replaces OU alone
  (clauses 10 and 11).
- End to end, `a_fleet_made_by_pki_init_admits_an_enrolled_host` in
  `crates/fleet-tools/tests/pki_e2e.rs` (clauses 7, 10 and 23):
  - `opamp-fleetctl pki init` makes the set;
  - a Server started on `server.toml.fragment` issues the Client's bootstrap pair before the Client
    starts;
  - the Server admits a Client started on `supervisor.toml.fragment` through `fleet-agent` once its
    enrolment is approved;
  - `fleet-server` and `fleet-agent` are development dependencies of `fleet-tools` and link into no
    artifact.
- `status_reads_the_server_hosts_certificates_and_the_server_makes_none`: the Server binary refuses
  `pki init`, `pki server-cert`, `pki bootstrap-cert` and `pki bootstrap-ca` as unknown commands.
  Its usage names `pki status` alone (clause 6).
- `the_server_warns_at_startup_about_a_ca_that_is_ending_and_serves_on` and
  `the_server_records_what_is_ending_and_what_has_ended` (clause 21):
  - a Server whose client CA ends within 30 days logs the warning and records `pki.expiring` at
    startup;
  - one whose CA has ended records `pki.expired` and keeps serving.
- `thirty_days_ahead_or_the_last_third_of_a_shorter_life` and
  `a_file_replaced_without_a_restart_neither_silences_the_warning_nor_goes_unsaid`: both
  `pki status` commands and the running Server judge one certificate alike (clauses 6 and 21).
- `a_certificate_s_end_is_read_and_put_in_one_line` and
  `both_status_commands_print_one_line_for_one_certificate`: one function in `fleet-core` reads a
  certificate's end, its days left and the line `pki status` prints for it. Both `pki status`
  commands print the same line for one certificate (clause 6).
- `status_exits_by_the_worst_end`,
  `status_lists_the_offline_certificates_and_a_fresh_set_is_not_ending` and
  `status_reads_the_server_hosts_certificates_and_the_server_makes_none`: each `pki status`
  lists each file with its serial and end. It exits `0`, `1` and `2` for a set with no, an ending
  and an ended certificate (clause 22).
- `scripts/dev-pki.sh` runs with no `openssl` on `PATH` (clause 23).
- `issuing_needs_the_key_and_a_request_within_bounds` and
  `a_certificate_lives_ten_days_by_default_at_most_ninety_and_never_past_its_ca`: an issuing
  request with `validity_days` of 0 or 91 is answered `400`. A certificate asked to outlive the
  bootstrap CA ends with it (clause 14).
- `bootstrap_ca_makes_a_new_ca_for_the_server`: `pki bootstrap-ca` writes a CA the Server starts on
  (clause 12).
- `a_host_certificate_is_named_by_its_ca_not_by_its_request`,
  `a_bootstrap_certificate_is_named_by_its_ca`,
  `a_leaf_is_named_for_its_ca_with_ca_turned_into_certificate` and
  `a_ca_whose_certificates_name_is_too_long_is_refused` (clause 19):
  - a host certificate the Server signs has the common name `<product> Client Certificate`, and a
    bootstrap certificate `<product> Bootstrap Certificate`;
  - under a client CA named `CN=Corp Root`, a host certificate is `CN=Corp Root Certificate`;
  - under a client CA whose derived name exceeds 64 characters, the Server refuses to start.
- `the_crate_declares_one_binary_and_no_library`: `crates/fleet-tools` declares exactly one binary
  target, `opamp-fleetctl`, and no library (clause 1).
- `the_operator_tool_is_built_for_four_targets_and_its_version_asserted_where_it_runs` and
  `each_archive_is_named_like_the_clients_packed_by_the_tool_and_checksummed` read the release
  workflow as data (clauses 2 and 3):
  - it builds `opamp-fleetctl` for exactly the four targets of clause 2 and asserts its version
    where the target runs;
  - it names each archive `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, packs it with
    `opamp-fleetctl package pack --format tar.gz`, and includes it in `SHA256SUMS`;
  - no `opamp-fleetctl_*_windows_*` file and no installer names it.
- `the_notes_upload_loop_selects_the_clients_archives_alone`: the release notes' upload loop, read
  out of the workflow, selects `supervisor_<version>_*.tar.gz` and matches no `opamp-fleetctl_`
  archive. `the_supervisor_passes_over_the_operator_tools_archives`: `package fetch --agent
  supervisor`, given a release that also holds `opamp-fleetctl_1.2.3_linux_amd64.tar.gz`, plans
  only the `supervisor_` archives (clause 4).
- `p12_extract_writes_the_certificate_and_its_key_the_key_owner_only`: a `bootstrap.p12` made by
  `fleet_core::pkcs12::file::seal` extracts to a `bootstrap.pem` and a `bootstrap-key.pem` whose
  key matches the certificate. The key is `0600` on Unix (clauses 24 and 26).
- `p12_extract_refuses_what_does_not_open_and_writes_nothing`: a wrong password, a file that is no
  bootstrap PKCS#12 file, and a file over the size bound are refused. Nothing is written
  (clause 24).
- `p12_extract_overwrites_neither_file`: an existing `bootstrap.pem` or `bootstrap-key.pem` in
  `--out` leaves both untouched, and the command fails naming it (clause 26).
- `the_password_comes_from_a_file_or_a_terminal_and_never_from_a_flag`: without a terminal and
  without `--password-file`, the command fails before it reads the file. No command-line flag
  takes a password value (clause 25).
- In the tests of `crates/fleet-tools/src/fetch.rs`,
  `a_link_out_of_the_tree_fails_the_repack_naming_it` and
  `a_link_out_of_the_tree_is_named_escaped` (clause 27):
  - a tree holding a link with an absolute target to an existing file outside it fails
    `pack_tree`;
  - so does a tree holding `../../` past the root to an existing file;
  - the error names the link, and no artifact is left at the output path;
  - the same two links with targets that exist nowhere fail the same way, so the outcome does not
    depend on the host.
- `a_link_inside_the_tree_is_packed_under_its_name`,
  `a_linked_directory_is_packed_under_both_names_and_a_cycle_does_not_hang` and
  `a_link_to_the_root_is_not_followed`: a link inside the tree to a file and to a directory is
  packed under its name with the contents. A link to a missing member inside the tree is dropped.
  A link to the root or above inside the tree is not descended into (clause 27).
- `a_link_the_kind_keeps_is_packed_as_that_link`: a link the kind names by path and target is
  packed as that link, with mode `0o777` and a zeroed time. The Client installing the tree with
  the same kind creates it (clause 27).
- `a_link_the_kind_does_not_keep_still_fails_the_repack`: each of these fails the repack, naming
  the link, with no artifact left (clause 27):
  - the same link without the kind naming it;
  - the named path with another target;
  - an unnamed absolute link beside a kept one;
  - the kept link reached through a linked directory.
- `only_the_named_path_with_the_named_target_is_kept` in `crates/fleet-util/src/tree.rs` decides
  the shared rule (clause 27).
- `a_repack_stores_normalized_modes`: a file with any execute bit is stored `0o755`, every other
  file `0o644`, a directory `0o755`, whatever write, setuid, setgid or sticky bits the tree carried
  (clause 27).
- `a_tree_whose_links_multiply_it_stops_at_the_member_cap`,
  `a_member_that_is_neither_file_nor_directory_fails_the_repack` and
  `a_repack_that_fails_while_writing_leaves_no_artifact` hold the member cap and the failure paths
  of the repack (clause 27).
- `the_same_tree_repacks_to_the_same_bytes`: packing the same tree twice yields byte-identical
  artifacts (clause 27).
- The tests that
  [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)
  names verify clauses 5 to 8, 12, 13 and 16 to 18 as they stand here. That includes the keys
  generated on a token. Among them are `init_with_the_ca_keys_on_the_token_leaves_both_cas_to_it`,
  `token_init_makes_the_keys_on_the_token_and_writes_none`,
  `a_new_bootstrap_ca_on_the_token_writes_its_certificate_alone`,
  `a_token_init_that_fails_partway_leaves_the_token_as_it_was` and
  `the_server_signs_with_what_token_init_made`.
- `a_later_repack_orders_between_the_version_and_the_next_release` (clause 28):
  - `--repack 1` keeps the version, and `--repack 2` of `2.14.3` is `2.14.4-0.r.2`;
  - the repacks order above `2.14.3` and below `2.14.4`, with `r.9` below `r.10`;
  - `--repack 0`, a version of other than three numbers, and a later repack of an agent the tool
    does not repack are refused.
- In `crates/fleet-tools/src/gpgv.rs`, with throwaway keys whose primary certifies and whose
  subkey signs (clause 29):
  - `a_detached_signature_verifies_only_by_a_pinned_key_over_its_bytes` and
    `a_clear_signed_text_yields_only_what_a_pinned_key_signed` refuse an altered byte. They
    refuse a key the keyring does not hold, and a key it holds but does not pin. They refuse a page
    in place of a signature. The clear-signed text comes back without its armour.
  - `a_signature_verdict_needs_a_pinned_primary_and_no_bad_signature` decides on `gpgv`'s status
    lines.
  - `a_detached_signature_needs_a_run_gpgv_calls_a_success` refuses a detached signature whose
    lines carry a pinned `VALIDSIG` and no `BADSIG` when `gpgv` failed.
- `the_version_flag_reports_the_baked_version`: the program reports the version it was built from
  (clause 2).

The test of the command tree, `the_program_has_two_trees_holding_the_operator_commands`, runs
`opamp-fleetctl --help`, `package --help` and `pki --help`. It verifies
[PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md), which states the tree.

**Not mechanically decidable:** review holds the following.

- No document of the project tells an operator to use an external tool. The operator manual
  describes issuing through the Server and names no other way to make a bootstrap certificate
  (clause 23). Review of every change to the manual holds this.
- The release notes describe the archives as the operator tool and not as packages (clause 4).
  Review of the release notes in the workflow holds this.
- `scripts/dev-pki.sh` and `scripts/seed_test_configs.sh` pass the development names of clause 20.
  Review holds this.
