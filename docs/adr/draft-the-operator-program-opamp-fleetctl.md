# ADR-draft-the-operator-program-opamp-fleetctl: The operator program `opamp-fleetctl`

- **Status:** 🟡 proposed
- **Date:** 2026-10-31
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-tools/` (the one program it builds, its `pki` commands in `src/pki.rs`, `p12-extract`, and the deterministic repack of `package fetch` in `src/fetch.rs`, `pack_tree` and `collect_members`), the dependencies of `crates/fleet-tools/Cargo.toml`, the Server's `pki status` command in `crates/fleet-server/src/main.rs` and the module that carries it, the expiry warnings of the running Server, the leaf naming of host and bootstrap certificates in `crates/fleet-server/src/ca.rs` and its startup check, the ending rule, the reading of a certificate's end with the line `pki status` prints for it, and the naming rule in `crates/fleet-core/`, the operator-tool build and its archives in `.github/workflows/release.yml`, the release notes, `scripts/dev-pki.sh` and what reads its output (`scripts/seed_test_configs.sh`, `.vscode/launch.json`, `.vscode/tasks.json`, the startup message of `crates/fleet-server/src/config.rs` that names it), every kind whose upstream tree the repack packs, and every document that names an operator command, tells an operator how to make, inspect or convert a certificate, or says what a repacked package carries

## Context

An Agent proves fleet membership with a client certificate in the TLS handshake and nothing else,
and the Server presents a certificate of its own on both planes
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md),
[ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)). That takes three certificate
authorities, each with one job: a server CA that signs the Server's certificate and a Gateway's
downstream certificate, the client CA the Server signs every host's certificate with, and a
bootstrap CA that signs the certificates a new host enrols with
([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). The Server makes every host
certificate itself, with `rcgen`, and issues every bootstrap certificate, and a host obtains its
first host certificate only through an enrolment an operator opens and approves. Everything
before that is the operator's: the three CAs, the Server's certificate and a Gateway's
certificate.

The certificates are one of three things this project makes before a fleet does anything useful;
a package signing key and signed package artifacts are the others (G-10, Q-1). Packing and signing
an artifact and fetching a release of a known agent are commands of the same operator program.
Unless a release carries that program, an operator who wants to roll out a single package needs
a checkout of this repository and a Rust toolchain.

The forces:

- **G-17** asks for a secured connection and an identified Agent, and **Q-1** for a system that is
  secure whatever its configuration. A fleet whose first step is a hand-written `openssl` command
  line is secure only as far as that command line is right: a missing `extendedKeyUsage`, a CA key
  in SEC1 form the Server cannot load, a bootstrap CA that shares its subject with the client CA,
  or a server certificate that names the wrong host each fail later and elsewhere.
- **The requirements are the project's own and are scattered.** The client CA's key must be
  PKCS#8, because `rcgen` loads nothing else. The bootstrap CA must have a subject of its own, or
  the Server refuses to start. The server certificate needs the loopback among its names for the
  loopback Operator plane to be reachable through a tunnel. A tool that knows these rules writes
  them once; a manual can only list them.
- **`openssl` is a system dependency of the setup, though not of the build.** The build keeps
  system libraries out (ADR-0022, on the crates issuance adds), and the operator's machine should
  not need one either. Its command-line behaviour differs between OpenSSL 1.1, 3.x and LibreSSL.
  OpenSSL 1.1 cannot read the PBES2/AES `bootstrap.p12` the Server hands out at all, and
  OpenSSL 3 needs the right options to open it.
- **Not every key belongs on the Server, and not every key off it.** Whoever holds the server CA's
  key can stand up a Server or a Gateway anywhere that every host trusts, so it stays on the
  operator's machine. The client CA's key has to be on the Server, because it signs renewals
  unattended. The bootstrap CA's key beside it adds nothing an attacker on the Server host does
  not already have: a bootstrap certificate opens nothing without an open window and an approval.
- **A CA lives as long as what replacing it costs justifies.** Replacing the server CA means a new
  `ca_file` on every host; replacing the client CA means every host enrolling again. Replacing the
  bootstrap CA touches only `server.toml` and the bootstrap certificates not yet used, so it can be
  replaced every year at little cost, and a key that is online is replaced on a schedule rather
  than only after a compromise.
- **A CA's end is not enforced where it would show first.** `rustls` treats a configured CA as a
  trust anchor, a name and a key without a validity, so a Server and its Clients keep accepting
  certificates under an expired CA. A browser on the Operator plane, `curl`, or another OpAMP
  implementation may refuse what this project's own ends accept, and without a warning the first
  sign of an expired CA or Server certificate is an outage somewhere else.
- **An audit reads certificates, not configuration.** An auditor shown a certificate asks who
  issued it and what it is for. Nothing reads a subject's common name to decide anything: a TLS
  client checks a server certificate's names against its subject alternative names
  (RFC 9525 section 2), and the Server tells an enrolment connection from a member's by its
  issuer, names a host by the SAN URI it puts in and decides a renewal by its proof
  ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md),
  [ADR-0015](0015-client-modes-and-the-gateway.md)). The subject is free to say what an auditor
  needs, and the name an operator wants there is that of their own installation, which no build
  constant knows.
- **The operator's own machine comes first.** It is where an operator works before any Server
  exists. Whoever holds the package signing key and can make an offer runs code on every host it
  reaches, which is why the Client installs only what a key the operator holds has signed
  ([ADR-0027](0027-packages-signatures-and-delivery.md)); the server CA's key is kept off the
  Server host for a like reason. A program that makes those keys is a program for the operator's
  machine, and the binary that serves the fleet has no use for it.
- **One audience, one job.** The certificates, the signing key, the packing and the fetch are all
  done once per fleet or once per release, by the same person, on the same machine. A program per
  task, or a Server subcommand tree for part of it, is names too many.
- **Keys need file modes.** Linux and macOS give `0600` and `0700` by `chmod`; Windows has no such
  modes, and an ACL that does the same is a piece of work of its own.
- **Names are a public contract,** and the obvious one is taken: `fleetctl` is the command-line
  tool of FleetDM, an actively maintained fleet-management product for the same audience, installed
  with `npm install -g fleetctl` and from its GitHub releases. Two `fleetctl` on one `PATH` cannot
  both be found, and a search for either finds the other.
- **The release is a contract too.** It is one workflow run from one version
  ([ADR-0034](0034-releases-attestations-and-the-server-image.md)), and every row of its build
  matrix already builds the packer for the runner it runs on, Windows included, because the
  Client's archives are packed by it.
- **Some places want a PEM pair while only a `bootstrap.p12` is at hand:** a host whose install is
  fed the pair, a tool that reads PEM only, an operator who wants to see what the file holds. The
  Client's install already reads the file with a bounded reader in the Client's crate, on the
  internal crate's PKCS#12 code ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)), and
  the operator program already depends on both crates.
- **Where an upstream publishes no artifact a Client can run as published,** the operator program
  unpacks it on the operator's host and repacks the tree before it is signed. A tree package
  carries no link ([ADR-0027](0027-packages-signatures-and-delivery.md)), so the repack
  dereferences links: a linked directory is packed under the linked name with its contents,
  because that name is one the agent uses (a tree may reach its Perl library through
  `usr/share/perl/5.26`, a link to `5.26.1`). Resolving a link with `std::fs::metadata` and
  reading it with `File::open` follows an absolute target and a `../..` target out of the tree
  into the operator host's own file system. Such a link would pack the operator's files into an
  artifact the operator signs and the fleet installs (disclosure, chosen by a compromised
  upstream whose download matches its published checksum), make the bytes depend on the
  operator's host (no determinism), and name a path of the host upstream built for, never the
  tree (no meaning). **Q-1** puts security before convenience.

This serves **G-17** (the connection is secured and the Agent is identified), **G-10** and
**Q-1** (secure by default).

This ADR restates [ADR-0032](0032-the-operator-program-opamp-fleetctl.md) in full. It changes where
a certificate's end is read: once, in `fleet-core`, with the line `pki status` prints for it,
rather than in each program. The reason ADR-0032 gave against that, the internal crate's first
`x509-parser` and `time` dependencies, has lapsed: `fleet-core` carries both, and the PEM reader
with them, for `x509` and `csr`, and the reading and the line were written twice, verbatim, in
`fleet-server` and `fleet-tools`.

## Decision

We will build the operator tools as one program, `opamp-fleetctl`, published with every release
for Linux and macOS, that makes the fleet's certificate authorities, the Server's and a Gateway's
certificate and its package signing key with `rcgen`, names every certificate by its product and
its role, says with the Server before any certificate ends, turns a bootstrap PKCS#12 file into
PEM, and repacks an upstream tree deterministically and from inside that tree alone, so that no
step of setting up or running a fleet needs `openssl` or another external tool.

1. **The crate builds exactly this program.** `crates/fleet-tools` has no library and one binary,
   `opamp-fleetctl`, and keeps the dependency direction of
   [ADR-0008](0008-five-crates-on-tokio-and-axum.md): it uses the Client's and the internal
   crate's items, and nothing depends on it. The program is `src/main.rs`, which parses the
   command line, with one module per command tree, `src/package.rs` and `src/pki.rs`; the release
   fetcher behind `package fetch` is `src/fetch.rs`.

2. **Built in the release run, from the release's version, for four targets.**
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, each in the row of the Client's build matrix for the same target. Each
   row whose target runs on its runner asserts that `opamp-fleetctl --version` reports the release
   version, as the Client's build does. The Windows row builds the program for its runner only as
   the packer of the Client's archive; no Windows archive of it is published.

3. **One archive per target, named like the Client's.**
   `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, by the asset-name grammar and the platform
   tokens of [ADR-0034](0034-releases-attestations-and-the-server-image.md), packed by
   `opamp-fleetctl package pack --format tar.gz` with the one member `opamp-fleetctl` and its
   executable mode. No `.deb`, `.rpm` or `.msi` carries it. The four archives are assets of the
   release beside the Client's and are covered by the same `SHA256SUMS`.

4. **It is not a package of the fleet.** The release notes list the four archives as the operator
   tool and say they are downloaded, not uploaded to the Server. Everything that selects the
   Client's archives selects them by their `supervisor_` stem, never by version and platform
   alone: the notes' upload loop, which would otherwise put an `opamp-fleetctl` archive into the
   `supervisor` Package as the entry of its platform, and `opamp-fleetctl package fetch --agent
   supervisor`. An upload made by hand anyway installs nothing: the Client's self-update refuses
   an artifact whose member is not `supervisor`
   ([ADR-0019](0019-the-installed-service-and-its-state.md)).

5. **No dependency beyond the issuance crates, and the Client's PKCS#12 reader.** The `pki`
   commands use `rcgen` with the features
   [ADR-0022](0022-bootstrap-certificates-and-enrolment.md) clause 13 states for issuance, and
   `time`. `p12-extract` also reads through the Client's
   `pkcs12_identity`, and so through `fleet_core::pkcs12::file` and `p12-keystore` (ADR-0022).
   Every key is ECDSA P-256 and written as PKCS#8 PEM, the form `[client_ca]` loads. Every
   certificate is PEM. Every serial is 16 random bytes, as the client CA's are.

6. **Three commands that make, and a status where the files are.** `opamp-fleetctl pki init` makes
   a fleet's certificates and its package signing key from nothing. `opamp-fleetctl pki
   server-cert` makes a server certificate from the existing server CA, for the Server or for a
   Gateway. `opamp-fleetctl pki bootstrap-ca` makes a new bootstrap CA to replace the Server's.
   `opamp-fleetctl pki status` says when the certificates in the offline directory end, and
   `server pki status` says when those of the Server host end. The Server's command runs and exits
   like `hash-credential` and `audit-verify`, before the Server starts serving. The Server binary
   carries no code that makes a CA or a server certificate. It signs host certificates
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and bootstrap
   certificates ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). When a certificate
   counts as ending (clause 21) is one rule in `fleet-core`, plain date arithmetic, and reading a
   certificate's end from its file, with the days it has left and the line `pki status` prints
   for it, is one function there, so both commands and the running Server read and judge alike.
   None of the commands makes a host certificate or a bootstrap certificate.

7. **`init` writes two directories, and the split is the point.** `--server-dir` receives what the
   Server host needs: `server.pem` and `server-key.pem`, `client-ca.pem` and `client-ca-key.pem`,
   `bootstrap-ca.pem` and `bootstrap-ca-key.pem`, and `server.toml.fragment` naming them in
   `[tls]`, `[client_ca]` and `[enrolment]`, including `bootstrap_ca_key_file`. `--offline-dir`
   receives what stays with the operator: `server-ca.pem` and `server-ca-key.pem`; the package
   signing key, an Ed25519 key in PKCS#8 as `package keygen` writes it, as
   `package-signing-key.pk8`, and its public half in hex as `package-signing-key.pub`
   ([ADR-0027](0027-packages-signatures-and-delivery.md)); and `supervisor.toml.fragment` naming
   `server-ca.pem` in `[tls] ca_file`, `bootstrap.pem` and `bootstrap-key.pem` under
   `--host-path` in `cert_file` and `key_file`, where a host puts the pair the Server issues it,
   and the public half in `[packages] verification_key`. No key of the server CA and no package
   signing key is ever written to `--server-dir`. Each fragment names its files by the directory
   they are meant to live in: `--server-path` (default: the absolute path of `--server-dir`) and
   `--host-path` (default: the absolute path of `--offline-dir`). The command prints which
   directory goes where.

8. **`init` is meant to run where the offline keys are to stay.** That is the operator's Linux or
   macOS machine, the one `opamp-fleetctl` is downloaded to. Run on the Server host, it is
   followed by moving `--offline-dir` off it, and the command says so when both directories are
   on one file system.

9. **The CAs are told apart by role and sign no CA.** The three CAs are `CN=<product> Server CA`,
   `CN=<product> Client CA` and `CN=<product> Bootstrap CA` (clause 17), so the bootstrap CA never
   shares a subject with the client CA
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). Each CA certificate is
   `CA:TRUE` with a path length of 0, `keyCertSign` and `cRLSign`.

10. **A server certificate names what is dialled in its SAN and its role in its subject.**
    `--name` is given at least once and takes a DNS name or an IP address; an empty value, a
    wildcard, or a name that is neither is refused. Each becomes a subject alternative name, and
    `IP:127.0.0.1` and `IP:::1` are always added, so the Operator plane on its loopback default is
    reachable through a tunnel with the same certificate. The certificate is `CA:FALSE`,
    `digitalSignature`, `serverAuth`. Its common name is derived from the server CA's
    (clause 19). `pki server-cert` takes O and OU from the server CA, and `--organization` and
    `--organizational-unit` replace either, so a Gateway run by another unit says so.

11. **`server-cert` serves the Server and a Gateway alike.**
    `opamp-fleetctl pki server-cert --offline-dir <dir> --name … --out <dir>` signs `server.pem`
    and `server-key.pem` in `--out` with `server-ca-key.pem`. For the Server they replace the files
    of `[tls]`, and the Server is restarted on them; for a Gateway they are its `[gateway.tls]`
    `cert_file` and `key_file` ([ADR-0015](0015-client-modes-and-the-gateway.md)), which the
    Agents behind it verify against the same `server-ca.pem`. The fleet's trust does not change,
    so no host's configuration does.

12. **A new bootstrap CA replaces the Server's.** `opamp-fleetctl pki bootstrap-ca --out <dir>`
    writes `bootstrap-ca.pem` and `bootstrap-ca-key.pem` to `--out`, with the subject a bootstrap
    CA carries (clause 9). The operator copies both to the Server host in place of
    `[enrolment]`'s files and restarts the Server. Bootstrap certificates from the old CA stop
    passing the handshake, and those not yet used are issued again.

13. **Lifetimes by default, each overridable, never past the CA.** The server CA and the client CA
    live 3650 days, the bootstrap CA 365 days, a server certificate 397 days. `init` takes
    `--ca-days` for the server CA and the client CA, `--bootstrap-ca-days` and `--server-days`.
    `server-cert` and `bootstrap-ca` take `--days` for the one certificate they make. A server
    certificate ends no later than the server CA that signs it, and one asked to outlive it is cut
    to the CA's end with a notice. Bootstrap certificates live as clause 14 states. Host
    certificates are the running Server's, signed for `[client_ca] validity_days` as
    [ADR-0021](0021-admission-by-client-certificate-and-revocation.md) states.

14. **A bootstrap certificate lives 10 days by default, at most 90, never past its CA.**
    `validity_days` of the issuing request runs from 1 to 90, and anything else is answered `400`.
    A certificate that would outlive the bootstrap CA ends with it, and the response's
    `not_after` says so.

15. **Nothing is overwritten, and nothing is half written.** A command that would write a file
    that exists refuses before it writes anything, naming the file. It writes into a new
    temporary directory beside its target and renames into place only once every file is
    complete. Directories it creates are `0700`; every key is `0600`.

16. **Keys are stored unencrypted.** The Server reads the client CA's key, the bootstrap CA's key
    and its own key unattended, and the offline keys are protected by where they are kept and by
    their mode, as every other key of the project is.

17. **The product is a flag, with a default.** `opamp-fleetctl pki init` and `pki bootstrap-ca`
    take `--product <name>` (default `OpAMP Fleet`): letters, digits, space, `.` and `-`, 1 to 40
    characters, no leading or trailing space. No build constant enters a certificate.

18. **The operator names an organization for audit.** `init` and `bootstrap-ca` take
    `--organization` (O) and `--organizational-unit` (OU), each optional, each 1 to 64 characters
    of UTF-8 without control characters (the upper bound of RFC 5280 Appendix A.1). `init` writes
    them into all three CAs and the server certificate. Attributes are encoded as `UTF8String`, in
    the order O, OU, CN. No other attribute is written.

19. **A leaf's common name is its CA's, with `CA` turned into `Certificate`.** Whoever signs a
    leaf — `opamp-fleetctl` a server certificate, the Server a host or a bootstrap certificate —
    takes the common name of the CA that signs it and replaces a trailing ` CA` with
    ` Certificate`, or appends ` Certificate` when there is none. A fleet made by `init` therefore
    has `<product> Server Certificate`, `<product> Client Certificate` and
    `<product> Bootstrap Certificate`, and a fleet whose CAs were made elsewhere gets a name from
    them without a setting of its own. The Server refuses to start when the name derived from the
    client CA or the bootstrap CA would exceed 64 characters.

20. **A development certificate says so.** `scripts/dev-pki.sh` makes its set with
    `--product "OpAMP Fleet Dev" --organization "Development"`, and
    `scripts/seed_test_configs.sh --enrol` issues its bootstrap certificate with the organization
    `Development`. Every certificate of a development set therefore reads `OpAMP Fleet Dev …` and
    `O=Development`, and one that turns up outside a developer's machine is recognized as such.

21. **The running Server warns before a certificate it depends on ends.** At startup and once a
    day while it runs, the Server reads the end of its own certificate (`[tls] cert_file`), of the
    client CA (`[tls] client_ca_file` and `[client_ca] cert_file`) and of the bootstrap CA
    (`[enrolment] bootstrap_ca_file`). One that ends within 30 days — or, for a certificate whose
    whole life is shorter than 90 days, within the last third of it — is *ending*: it is logged as
    a warning naming the file, its subject and the date, and recorded in the audit as
    `pki.expiring`; one that has ended is logged as an error and recorded as `pki.expired`
    ([ADR-0023](0023-an-append-only-audit-record.md)). Neither stops the Server: the hosts it
    serves decide for themselves, and a Server that refuses to start cannot be told to renew.

22. **`pki status` says when each certificate ends.** `server pki status --config <server.toml>`
    lists the files of clause 21. `opamp-fleetctl pki status --offline-dir <dir>` lists the server
    CA. Each prints every certificate with its subject, its serial, its end and the days left, and
    exits `0` when none is ending in the sense of clause 21, `1` when one is, and `2` when one has
    ended, so a monitoring job can run it.

23. **The project uses its own commands.** `scripts/dev-pki.sh` makes its development set with
    `cargo run -p fleet-tools -- pki init`, writes a complete `server.toml` and `supervisor.toml`
    from the fragments, and needs no `openssl`. Its development Client holds a bootstrap pair the
    development Server issued: `scripts/seed_test_configs.sh --enrol` asks the Server for it,
    writes it where `supervisor.toml` names it, opens the window and approves the request. The
    operator manual describes the `pki` commands and the issuing route, reads a serial with
    `pki status` or from the route's answer, and names no external tool. Certificates made
    elsewhere keep working: the Server reads any PEM that meets the requirements these commands
    write down.

24. **`p12-extract` reads what the Client's install reads.**
    `opamp-fleetctl pki p12-extract <file> --out <dir>` opens `<file>` with
    `fleet_agent::tls::pkcs12_identity`, the reader the Client's install uses. It therefore takes
    exactly the bootstrap PKCS#12 files
    [ADR-0022](0022-bootstrap-certificates-and-enrolment.md) clauses 4 and 15 describe, by the
    file's form and by how the Client reads it, and refuses the rest, naming
    why. That covers a file over the reader's size bound, a wrong password, a scheme other than
    PBES2/AES with an HMAC-SHA-2 MAC, and anything other than one key with its one certificate.

25. **Where the password comes from.** The password is taken from a hidden question on a
    terminal, or from `--password-file <path>`, which holds the password on its first line. It is
    never taken from a command-line value or an environment variable, which other users or the
    shell history could see. Without a terminal and without `--password-file`, the command fails
    before it reads the file.

26. **What `p12-extract` writes.** It writes `<dir>/bootstrap.pem` (the certificate) and
    `<dir>/bootstrap-key.pem` (the key as unencrypted PKCS#8). These are the names of the pair the
    Client's install finds ([ADR-0019](0019-the-installed-service-and-its-state.md)). On Unix the
    key is written `0600`. It creates `<dir>` when it is missing and overwrites nothing
    (clause 15): if either file exists, it writes neither and names the one in the way. It prints
    the certificate's subject, serial and `notAfter`, and never the key or the password.

27. **Every repack `opamp-fleetctl package fetch` makes is deterministic, and it carries nothing
    from outside its tree.** Where an upstream publishes no artifact a Client can run as
    published, the tool repacks it on the operator's host, before it is signed, into a `.tar.gz`
    that keeps file modes. The repack has a fixed member order and zeroed times and ownership, and
    it carries no link.
    - **A link is resolved inside the tree.** Its target is taken relative to the directory holding
      the link, and every link met on the way is resolved by the same rule. A link that resolves to
      a file or a directory inside the tree is dereferenced: a linked directory is packed under the
      linked name too, with its contents.
    - **A link that leads out of the tree fails the repack.** A link leads out when its target is
      absolute, or when resolving it climbs above the root of the tree, whether or not anything
      exists at the path it would name on the operator's host. The repack stops before it writes
      the artifact and names every such link by its path in the tree and its target. Nothing of
      the operator's host is read through it.
    - **A link that names nothing inside the tree is dropped.**
    - **A link leading back up the tree is not followed,** the root of the tree included: a link to
      a directory the walk is already inside is packed as that name and not descended into.
    - The member cap a tree package has
      ([ADR-0027](0027-packages-signatures-and-delivery.md)) is checked while packing.

    Repacking the same release therefore yields the same bytes and the same hash on any
    operator's host, and no accidental rollout.

**Out of scope:** the names of the command trees of `opamp-fleetctl`, which commands each holds,
and the absence of any other program name for them, which
[ADR-0028](0028-package-versions-high-water-mark-and-floor.md) states; issuing a host
certificate by any route other than enrolment, which the specification rules out; how the Server
issues, records, hands out and revokes bootstrap certificates, and what of their subject a host
certificate carries ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)); customers as a
resource of the Server; attributes other than O, OU and CN in what `pki init`, `pki server-cert`
and `pki bootstrap-ca` write; encrypting keys at rest, a hardware security module, or a passphrase
prompt; replacing the server CA or the client CA, which every host's configuration or enrolment
follows, and holding two bootstrap CAs at once while one replaces the other; enforcing a CA's end
in the handshake, or limiting the running Server's signatures to its CA's end; certificate
revocation lists or OCSP; ACME and certificates from a public CA, which the Server reads as they
are; a `pki` command on the Client binary; the Server's `pki status` on Windows or macOS, where the
Server does not run; the operator tool on Windows; native packages, Homebrew, `npm` or any other
installer; provenance of the release's assets
([ADR-0034](0034-releases-attestations-and-the-server-image.md)); commands that drive the running
fleet through the Operator plane beyond the upload `package fetch` makes; writing the extracted
key encrypted, extracting files other than the bootstrap form, and making a PKCS#12 file with the
tool; the file modes the repack keeps, including group- and world-writable bits a tree may carry,
which the repack leaves as upstream set them and the Client applies as
[ADR-0037](0037-the-accounts-the-service-runs-under.md) clause 23 states; a kind's own removal,
before packing, of links it
knows upstream leaves behind (as an AppImage's repack removes `.DirIcon`), which each kind states
where it is decided; hard links, which are files of the tree; and the trust in the extracted tree's
origin, which the hash of what was downloaded settles.

## Alternatives considered

- **Keep `openssl` and document it better.** Rejected: the rules stay in prose an operator has to
  get right by hand, and a system tool whose behaviour differs between versions stays a
  prerequisite of every fleet. For a `bootstrap.p12`, OpenSSL 1.1 cannot open the file, and with
  OpenSSL 3 the operator has to know the options.
- **All the commands on the Server binary,** beside `pki status`, `hash-credential` and
  `audit-verify`. The Server binary is certain to be at hand, but it is the binary of the host the
  package signing key and the server CA's key are kept away from, and the archive writers and the
  release fetcher would be linked into the binary that faces the fleet — attack surface with no use
  in serving, since the Server never creates or opens an artifact
  ([ADR-0027](0027-packages-signatures-and-delivery.md)). With the operator tool published, the
  one argument for it — no second binary to obtain — is gone; the version both share is the
  release's.
- **Keep the bootstrap CA offline and make bootstrap certificates with `opamp-fleetctl`.** It gives
  each customer a subject of its own, but each certificate is a trip to the offline machine, and
  the Server neither lists nor knows what was issued until a host presents it. It protects a key
  whose loss is already outweighed by the client CA's key on the Server.
- **One CA for client and bootstrap certificates, with the bootstrap ones marked by an extension
  or found in a register.** It saves one key pair, but it makes admission depend on a second check
  that lets a host in when it fails. With a CA of its own a failure falls to the safe side: an
  enrolment connection.
- **One CA for everything.** It combines the above with the server CA's key online: a copy of the
  Server's key would then impersonate the Server to every host.
- **Country, state and locality in the subjects `pki init` writes as well.** They describe where an
  organization sits, which an audit of the CAs and the server certificate does not ask, and
  `country` needs an encoding of its own (`PrintableString`). O and OU name who is responsible.
- **The product name from the build (`PRODUCT_TITLE`).** It needs no flag, but it names the Client
  product (`OpAMP Fleet Agent`), lives only in `crates/fleet-agent`, and is the same for every
  installation of one build. An operator's certificates name their installation.
- **A slug for the fleet (`--fleet opamp-fleet`) and the role in lower case.** The smallest
  naming, but an auditor reads a slug and no organization, and no leaf shows its role in its
  subject.
- **A configuration file for issuer and subject data,** or a setting for the leaf names in
  `server.toml`. Either holds everything in one place, but three values are set at most once a
  year, a file has to be versioned, validated and documented, and leaf names set apart from the
  CA's can drift from it. An operator with richer requirements makes the certificates in their own
  PKI, which the Server accepts (clause 23).
- **Shorter lifetimes for the server CA and the client CA too.** The online client CA's key
  would be replaced more often, but replacing it means every host enrols again, and replacing the
  server CA means a new `ca_file` on every host. Without a way to hold two CAs at once, both stay
  at ten years.
- **Leave the package signing key to `package keygen` alone.** It keeps `init` to certificates,
  but a fleet that skips the second step has no key, and its Clients take no package until
  someone sets `verification_key` by hand. The key belongs where `init` already puts what stays
  offline, and `keygen` remains for a second key or a replacement.
- **Each program reads the certificate itself, the date rule alone in `fleet-core`.** It kept the
  internal crate free of `x509-parser`, `time` and a PEM reader while it had none, but the reading
  and the line `pki status` prints were then written twice, verbatim, and `fleet-core` carries
  those dependencies for `x509` and `csr` anyway.
- **`fleet-tools` depending on `fleet-server` for the rule.** No new code in `fleet-core`, but the
  operator tool would link the Server's library, its router included, for a dozen lines.
- **`pki status` in one program only.** The Server host does not hold the server CA's certificate
  and the operator's machine does not hold `server.toml`, so either program alone sees only part of
  the certificates, and the server CA's end — which every host's trust hangs on — would be visible
  nowhere.
- **Issue a host certificate from the command line**, offline with the client CA's key or through
  a route of the running Server. Rejected: the specification has a host obtain its certificate
  only through an approved, time-limited enrolment over the protocol's own means, never through a
  side channel, and a route would let the operator credential alone mint a member.
- **Limit every signature of the running Server to its CA's end, and refuse renewals under an
  ended CA.** It belongs with how the Server signs
  ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)); this decision warns
  instead (clauses 21 and 22).
- **Encrypt the offline CA keys with a passphrase.** It needs an encryption crate beyond clause 5
  and a prompt the commands must handle; kept out of scope, not ruled out.
- **Recommend `step` or `cfssl`.** Either makes certificates well, but each is one more tool to
  install and to learn, and neither knows this project's requirements.
- **Two programs, one for packages and one for certificates, both published.** It lifts the
  toolchain requirement, but leaves two names for one person's one job.
- **Put the commands into the Client binary.** The Client is published already, but it lands on
  every host, and every host would then carry the means to make a CA and a signing key.
- **Name it `fleetctl`.** The `kubectl` analogy the crate split already draws, but the name belongs
  to FleetDM's tool (Context).
- **Name it `opamp-operator` or `opamp-fleet`.** The first says who uses it, but a name ending in
  `-ctl` says it is the command-line tool of a system, which is what an operator looks for. The
  second is the build-time product name the Client's service is registered under
  ([ADR-0019](0019-the-installed-service-and-its-state.md)), so a program of that name reads as
  the Client.
- **All five targets, Windows included.** Windows would need its own way to keep a key readable by
  its owner alone, a piece of work this decision does not take on.
- **Linux only.** Simpler by two builds, but an operator on macOS — a likely operator machine —
  would again need a toolchain, and macOS gives the same file modes as Linux.
- **Keep the tools out of the release.** Every operator builds them from a checkout, and a fleet
  cannot install a single signed package without a Rust toolchain on someone's machine.
- **`p12-extract` writes the key encrypted (PKCS#8 `ENCRYPTED PRIVATE KEY`).** The key stays
  protected at rest. But the Client's install reads the pair in the clear
  ([ADR-0019](0019-the-installed-service-and-its-state.md)), and neither `rustls` nor `rcgen`
  reads an encrypted key. Owner-only permissions are what every other key is given
  ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).
- **`p12-extract` writes the certificate only.** The key never leaves the file. Not chosen: the
  operator asked for the key, and the PEM pair is a form the Client takes.
- **Read any PKCS#12 file (other schemes, several certificates).** This is more general, but needs
  a second reader with weaker schemes. The bootstrap file is the only PKCS#12 file this project
  makes.
- **A command of its own outside `pki` (for example `opamp-fleetctl p12 extract`).** A third tree
  for one command. The certificates already live under `pki`.
- **The password as `--password <value>`.** It would be visible in `ps` and in the shell history.
- **The repack follows every link, wherever it leads.** Simple, and close to `tar --dereference`.
  Rejected: it ships the operator's files to the fleet under a signature, and it makes the bytes
  depend on the operator's host, which breaks the repack's own promise of determinism.
- **Drop a link that leads out of the tree, and report it.** Nothing leaks either, the outcome is
  host-independent if the test is the same one, and a tree with stray links still repacks. Rejected
  as the default: the artifact then silently differs from the tree upstream built — a line on the
  operator's terminal, before a signature, is easily missed and never reaches the fleet — and an
  agent that needed the path fails on every host it is rolled out to rather than on the one host
  where it was packed. Failing puts the decision before the signature, where the operator is. A
  kind whose upstream reliably leaves such links removes them by name before packing, which is a
  reviewed, per-kind choice rather than a general silence. Dropping is cheaper to the operator;
  hardening outranks that.
- **Pack the out-of-tree link as a link.** Faithful to upstream, but a tree package refuses every
  link ([ADR-0027](0027-packages-signatures-and-delivery.md)), so the Client would refuse the
  artifact on every host. Rejected.
- **Decide by the canonical path on the operator's host** (`canonicalize` the target and compare it
  with the canonical root). Simple to write, but a target that does not exist cannot be
  canonicalized, so a dangling absolute link would be dropped on one host and refused on another,
  and the result would again depend on the host. Rejected for the rule; it may still serve inside
  the implementation for targets inside the tree.
- **Confine the walk with the operating system** (`openat2` with `RESOLVE_BENEATH`, or a chroot).
  Strong where it exists, but Linux-only, and the repack also runs on macOS. Left to the
  implementation as a further guard, not the rule.

## Sources / Prior art

- HashiCorp Consul, `consul tls ca create`: a self-signed CA made by the product's own binary, with
  a days option and a default common name.
  <https://developer.hashicorp.com/consul/commands/tls/ca>
- HashiCorp Nomad, `nomad tls ca create` and `nomad tls cert create`: a CA and server, client and
  CLI certificates from the product's own binary.
  <https://developer.hashicorp.com/nomad/commands/tls/ca-create>,
  <https://developer.hashicorp.com/nomad/docs/commands/tls/cert-create>
- HashiCorp Nomad and Consul put their certificate commands into the one binary that also runs the
  servers (`nomad tls`, `consul tls`) — the arrangement this decision does not follow, because here
  the operator's offline keys are to stay off the server host.
- Kubernetes, `kubeadm init phase certs ca`: the cluster's CAs generated by the setup tool itself,
  skipping files that exist.
  <https://kubernetes.io/docs/reference/setup-tools/kubeadm/kubeadm-init-phase>
- Kubernetes TLS bootstrapping: a short-lived bootstrap token issued by the control plane, which
  approves the CSR it carries.
  <https://kubernetes.io/docs/reference/access-authn-authz/kubelet-tls-bootstrapping/>
- Kubernetes PKI: the CAs are named for their role (`kubernetes`, `etcd-ca`, `front-proxy-ca`),
  and the kubelet's client signer permits one subject only, `O=system:nodes`,
  `CN=system:node:<name>`, whatever the CSR would like.
  <https://kubernetes.io/docs/setup/best-practices/certificates/>,
  <https://kubernetes.io/docs/reference/access-authn-authz/certificate-signing-requests/>
- Kubernetes publishes `kubectl` per platform as a download of its own, apart from the components
  that run on a node or the control plane. <https://kubernetes.io/releases/download/>
- HashiCorp Vault's PKI engine: an issuing CA online, key pairs generated on request and returned
  once, a root kept offline. <https://developer.hashicorp.com/vault/docs/secrets/pki>
- `rcgen`: `CertificateParams::self_signed` and `signed_by`, `BasicConstraints::Constrained`.
  <https://docs.rs/rcgen>
- `rustls-pki-types` `TrustAnchor`: a subject, a key and name constraints, no validity — why an
  expired CA goes unnoticed by this project's own ends. <https://docs.rs/rustls-pki-types>
- RFC 5280 section 4.1.2.4, RFC 5280 section 4.1.2.6 and Appendix A.1: the issuer and subject,
  the attribute types, and the upper bound of 64 characters for organization, organizational unit
  and common name.
  <https://www.rfc-editor.org/rfc/rfc5280>
- RFC 9525 section 2: the common name must not be used to identify a service.
  <https://www.rfc-editor.org/rfc/rfc9525>
- CA/Browser Forum Baseline Requirements, section 7.1.4: a publicly trusted server certificate's
  common name, when present, repeats one of its SANs. The rule binds public CAs only; the server
  CA here is private. <https://cabforum.org/working-groups/server/baseline-requirements/>
- FleetDM's `fleetctl`: installed with `npm install -g fleetctl`, from an install script, or from
  its GitHub releases. <https://fleetdm.com/guides/fleetctl>,
  <https://www.npmjs.com/package/fleetctl>
- `openssl pkcs12 -in bootstrap.p12 -nodes` (OpenSSL 3 man page `openssl-pkcs12`): the
  established way to turn a PKCS#12 file into PEM. It reads PBES2/AES files only from OpenSSL 3
  on. Consulted from memory.
- rsync's `--safe-links` (ignore a link that points outside the copied tree) and
  `--copy-unsafe-links` (copy what it points to): two established answers to the same question,
  the first close to the dropping alternative. Recalled from the rsync manual, not re-read.
- GNU tar's `--dereference` (`-h`), which the repack follows for links inside the tree and which
  follows a link wherever it leads. Recalled, not re-read.
- CWE-59, *Improper Link Resolution Before File Access ('Link Following')*: the class an
  out-of-tree link belongs to. Recalled, not re-read.
- Reproducible Builds (reproducible-builds.org): an artifact's bytes must not depend on the build
  host's file system beyond its declared inputs. Recalled, not re-read.
- A wrapped kind's AppImage repack, on that kind's branch, whose extraction of an AppImage built
  from Debian packages leaves links pointing at files no tree carries. Whether any of them is
  absolute is not verified.
- This project's own requirements:
  [ADR-0021](0021-admission-by-client-certificate-and-revocation.md) on admission, the client CA
  and how the Server signs;
  [ADR-0022](0022-bootstrap-certificates-and-enrolment.md) on the bootstrap CA, the crates
  issuance adds, the `bootstrap.p12` file and its reader;
  [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) on the planes;
  `docs/SPECIFICATION.md` on how an Agent obtains its certificate;
  [ADR-0027](0027-packages-signatures-and-delivery.md) on why a package is signed by a key the
  operator holds and what a tree package may carry;
  [ADR-0019](0019-the-installed-service-and-its-state.md) on the member a package artifact
  carries; [ADR-0034](0034-releases-attestations-and-the-server-image.md) on the release, its
  naming and its assets.

## Consequences

- Positive: a fleet is set up with two binaries the project builds and nothing else (G-17, Q-1);
  the requirements a hand-made certificate can miss are written once, in code that is tested; the
  offline keys have a place of their own from the first command on; an ending CA or server
  certificate is announced 30 days ahead in the log, the audit and a command a monitor can run.
- Positive: an operator downloads one program and can make the fleet's certificates, a signing
  key and signed packages without a toolchain (G-10, G-17, Q-1); the two keys that stay on the
  operator's machine, the server CA's and the package signing key, each do what nothing on the
  Server host can; the Server binary carries no command that makes them; the packer the release
  uses is the one an operator downloads.
- Positive: the bootstrap CA is replaced every year at the cost of one restart and the unused
  bootstrap certificates.
- Positive: every certificate says what it is for in plain words, and each CA and server
  certificate names the organization behind it, so an audit reads both from the certificate
  presented; a fleet with CAs from its own PKI gets leaf names that match them without a setting;
  a development certificate is told from a production one by its subject alone.
- Positive: an operator turns a `bootstrap.p12` into the PEM pair with the project's own tool, on
  Linux and macOS, without OpenSSL.
- Positive: no file of the operator's host reaches a signed artifact through a link, and the bytes
  of a repack depend on the release alone, so a repack is deterministic across operators; the
  operator learns of an out-of-tree link before signing, by name, on the host where it can be
  judged.
- Negative / trade-offs: the Server binary carries `pki status` beside serving, and the operator
  tool a second job beside packages; a fresh development set needs a running Server before its
  Client can start, and the development Client enrols, which adds an approval to a fresh
  development setup; keys at rest are only as safe as the place they are kept; a host certificate
  signed by the running Server can still outlive its CA, which the warnings announce rather than
  prevent.
- Negative / trade-offs: a server certificate's common name is not one of its names, so it could
  not be issued by a public CA in this form; it never is, the server CA being the fleet's own.
- Negative / trade-offs: four more builds and four more assets per release; the release carries a
  program that makes keys; an operator on Windows still builds from a checkout.
- Negative / trade-offs: an extracted key lies in the clear on whatever disk `--out` names, as
  the PEM pair the Client's install takes does. Protecting that directory is the operator's job.
- Negative / trade-offs: an upstream tree with an absolute or escaping link does not repack until
  the kind removes that link by name. If an AppImage's Debian leftovers are absolute, that
  kind's repack has to name them, which the kind's change does with its tests. Resolving a link
  component by component inside the tree is more code than calling `metadata`; it is bounded by
  the member cap and by the ancestor guard against cycles.
- Follow-ups: encrypting the offline keys; replacing a CA without every host enrolling again, and
  holding two bootstrap CAs while one replaces the other; enforcing a CA's end in the handshake,
  or limiting the running Server's signatures to it; showing the issuer's O and OU wherever the
  Server lists certificates; a view of the certificates' ends in the bundled UI; the operator tool
  on Windows with key files only their owner can read; a command tree for the running fleet, if
  one is ever wanted, under this program's name; whether the Enrolments tab should offer the PEM
  pair, now that the tool can make it; whether a repack keeps a group- or world-writable mode as
  upstream set it or normalizes modes (for example to `0755`/`0644`), together with how the Client
  applies a tar's modes, and whether the Client's tree extraction should clear those bits
  regardless of how the artifact was built.

## Enforcement

Tests that carry `Verifies: ADR-0032`:

- `pki init` writes exactly the files of clause 7 into the two directories: the bootstrap CA's key
  into `--server-dir`, no server-CA key and no package signing key into it,
  `package-signing-key.pk8` (owner-only) and `package-signing-key.pub` into `--offline-dir` alone,
  keys `0600`, directories `0700`, fragments that name `--server-path` and `--host-path`; the key
  signs a package the Client's verifier accepts under the `verification_key` the fragment names; a
  second run over the same directories is refused without writing anything (clauses 7 and 15).
- The bootstrap CA ends 365 days after it starts, and the server CA and the client CA 3650; a
  server certificate asked to outlive its CA ends with the CA (clause 13).
- `pki init --product "Acme Fleet" --organization "Acme AG" --organizational-unit "IT"` writes
  CAs whose subjects are `O=Acme AG, OU=IT, CN=Acme Fleet Server CA`, `… Client CA` and
  `… Bootstrap CA`, each with a path length of 0 and passing the Server's startup check, and a
  server certificate `O=Acme AG, OU=IT, CN=Acme Fleet Server Certificate` with every `--name` and
  the loopback as SANs. Without the flags the CAs are `CN=OpAMP Fleet … CA` and carry no O or OU.
  `--fleet` is an unknown option, and a product or an organization outside its grammar, a wildcard
  or an empty `--name` is refused (clauses 9, 10, 17 and 18).
- `pki server-cert` signs with the offline CA, a Client that trusts the old `server-ca.pem` accepts
  the new server certificate, and the certificate copies O and OU from the server CA, of which
  `--organizational-unit` replaces OU alone (clauses 10 and 11).
- End to end, in `crates/fleet-tools/tests/`: `opamp-fleetctl pki init` makes the set, a Server
  started on `server.toml.fragment` — `fleet-server` as a development dependency of `fleet-tools`,
  which links into no artifact — issues the Client's bootstrap pair before the Client starts, and
  admits a Client started on `supervisor.toml.fragment` through `fleet-agent` once its enrolment is
  approved (clauses 7, 10 and 23).
- The Server binary refuses `pki init`, `pki server-cert`, `pki bootstrap-cert` and
  `pki bootstrap-ca` as unknown commands, and its usage names `pki status` alone (clause 6).
- A Server whose client CA ends within 30 days logs the warning and records `pki.expiring` at
  startup, and one whose CA has ended records `pki.expired` and keeps serving (clause 21).
- Both `pki status` commands, and the running Server, judge one certificate alike (clauses 6
  and 21).
- Each `pki status` lists each file with its serial and end, and exits `0`, `1` and `2` for a set
  with no, an ending and an ended certificate (clause 22).
- `scripts/dev-pki.sh` runs with no `openssl` on `PATH` (clause 23).
- An issuing request with `validity_days` of 0 or 91 is answered `400`, and a certificate asked to
  outlive the bootstrap CA ends with it (clause 14).
- `pki bootstrap-ca` writes a CA the Server starts on (clause 12).
- A host certificate the Server signs has the common name `<product> Client Certificate`, and a
  bootstrap certificate `<product> Bootstrap Certificate`; under a client CA named
  `CN=Corp Root`, a host certificate is `CN=Corp Root Certificate`, and under a client CA whose
  derived name exceeds 64 characters the Server refuses to start (clause 19).
- The crate: `crates/fleet-tools` declares exactly one binary target, `opamp-fleetctl`, and no
  library (clause 1).
- The release workflow, read as data: it builds `opamp-fleetctl` for exactly the four targets of
  clause 2, asserts its version where the target runs, names each archive
  `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, packs it with `opamp-fleetctl package pack
  --format tar.gz`, and includes it in `SHA256SUMS`; no `opamp-fleetctl_*_windows_*` asset and no
  installer names it (clauses 2 and 3).
- The release notes' upload loop, read out of the workflow, selects `supervisor_<version>_*.tar.gz`
  and matches no `opamp-fleetctl_` archive, and `package fetch --agent supervisor`, given a release
  that also holds `opamp-fleetctl_1.2.3_linux_amd64.tar.gz`, plans only the `supervisor_` archives
  (clause 4).
- A `bootstrap.p12` made by `fleet_core::pkcs12::file::seal` extracts to a `bootstrap.pem` and a
  `bootstrap-key.pem` whose key matches the certificate. The key is `0600` on Unix (clauses 24
  and 26).
- A wrong password, a file that is no bootstrap PKCS#12 file, and a file over the size bound are
  refused, and nothing is written (clause 24).
- An existing `bootstrap.pem` or `bootstrap-key.pem` in `--out` leaves both untouched, and the
  command fails naming it (clause 26).
- Without a terminal and without `--password-file` the command fails before it reads the file, and
  no command-line flag takes a password value (clause 25).
- In the tests of `crates/fleet-tools/src/fetch.rs`: a tree holding a link with an absolute target
  to an existing file outside it, and one holding `../../` past the root to an existing file, each
  fail `pack_tree`, the error names the link, and no artifact is left at the output path; the same
  two links with targets that exist nowhere fail the same way, so the outcome does not depend on
  the host (clause 27).
- A link inside the tree to a file and to a directory is still packed under its name with the
  contents, a link to a missing member inside the tree is dropped, and a link to the root or above
  inside the tree is not descended into:
  `a_linked_directory_is_packed_under_both_names_and_a_cycle_does_not_hang` and
  `a_link_to_the_root_is_not_followed` (clause 27).
- Packing the same tree twice yields byte-identical artifacts (clause 27).

The test of the command tree, `opamp-fleetctl --help`, `package --help` and `pki --help`, verifies
[ADR-0028](0028-package-versions-high-water-mark-and-floor.md), which states the tree.

**Not mechanically decidable:** that no document of the project tells an operator to use an
external tool, and that the operator manual describes issuing through the Server and names no
other way to make a bootstrap certificate (clause 23), are held by review of every change to the
manual; that the release notes describe the archives as the operator tool and not as packages
(clause 4) by review of the release notes in the workflow; that `scripts/dev-pki.sh` and
`scripts/seed_test_configs.sh` pass the development names of clause 20, by review.
