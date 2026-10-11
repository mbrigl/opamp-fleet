# ADR-0028: The operator tools are one program, `opamp-fleetctl`, published with every release for Linux and macOS, that makes the fleet's certificate authorities, the Server's certificate and the bootstrap certificate, and the Server says before any of them ends

- **Status:** 🟢 accepted
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** the `pki` commands of `opamp-fleetctl` in `crates/fleet-tools/` and the module that carries them, the Server's `pki status` command in `crates/fleet-server/src/main.rs` and the module that carries it, the ending rule in `crates/fleet-core/`, the expiry warnings of the running Server, `scripts/dev-pki.sh` and what reads its output (`scripts/seed_test_configs.sh`, `.vscode/launch.json`, `.vscode/tasks.json`, the startup message of `crates/fleet-server/src/config.rs` that names it), and every document that tells an operator how to make or inspect a certificate; `crates/fleet-tools/` (the program it builds, its command tree and the names of its commands), the operator-tool build and its archives in `.github/workflows/release.yml`, the release notes, and every document or script that names an operator command

## Context

An Agent proves fleet membership with a client certificate in the TLS handshake and nothing else,
and the Server presents a certificate of its own on both planes
([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md),
[ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)).
That takes three certificate authorities, each with one job: a server CA that signs the Server's
certificate and a Gateway's downstream certificate, the client CA the Server signs every host's
certificate with (ADR-0021 clause 9), and a bootstrap CA that signs the one certificate a new host
enrols with (ADR-0021 clause 19). Every host certificate is already made by the Server itself,
with `rcgen` (ADR-0021 clause 12), and a host obtains its first one only through an enrolment an
operator opens and approves. Everything before that is left to the operator: the three CAs, the
Server's certificate, a Gateway's certificate and the bootstrap certificate. The project's only
help is `scripts/dev-pki.sh`, which calls the `openssl` command-line tool, and the operator manual
repeats `openssl` commands for production and for reading a certificate's serial.

The certificates are one of three things this project makes before a fleet does anything useful;
a package signing key and signed package artifacts are the others (G-10, Q-1). Packing and signing
an artifact and fetching a release of a known agent are commands built from `crates/fleet-tools`.
Unless a release carries them, an operator who wants to roll out a single package needs a checkout
of this repository and a Rust toolchain on their own machine.

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
  system libraries out (ADR-0021 clause 12), and the operator's machine should not need one
  either. Its command-line behaviour also differs between OpenSSL 1.1, 3.x and LibreSSL.
- **Not every key belongs on the Server.** Whoever holds the server CA's key can impersonate the
  Server to every host; whoever holds the bootstrap CA's key can mint bootstrap certificates. The
  client CA's key has to be on the Server, because it signs renewals unattended. The other two do
  not.
- **A CA's end is not enforced where it would show first.** `rustls` treats a configured CA as a
  trust anchor, a name and a key without a validity, so a Server and its Clients keep accepting
  certificates under an expired CA. A browser on the Operator plane, `curl`, or another OpAMP
  implementation may refuse what this project's own ends accept, and nothing tells the operator
  that a CA or the Server's own certificate is about to end. The first sign of an expired CA is an
  outage somewhere else.
- **The operator's own machine comes first.** It is where an operator works before any Server
  exists; the Server binary runs on the Server host, where two of the three CA keys are not to be.
- **The keys these tools make do not belong on the Server host.** Whoever holds the package
  signing key and can make an offer runs code on every host it reaches, which is why the Client
  installs only what a key the operator holds has signed
  ([ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)). The
  server CA's and the bootstrap CA's keys are kept off the Server host for the same reason. A
  program that makes those keys is a program for the operator's machine, and the
  binary that serves the fleet has no use for it.
- **The Server binary is the one program an operator certainly has,** an argument for making
  certificates there rather than in one more binary to build. It holds only while no operator tool
  is published; once one is, it is the program an operator downloads first, before any Server or
  Client exists.
- **One audience, one job.** The certificates, the signing key, the packing and the fetch are all
  done once per fleet or once per release, by the same person, on the same machine. A program per
  task, or a Server subcommand tree for part of it, is names too many, and certificate commands
  have no natural home in a program named after packages.
- **Keys need file modes.** Every key these commands write is `0600` in a `0700` directory
  (clause 10). Linux and macOS give that by `chmod`; Windows has no such modes, and an ACL
  that does the same is a piece of work of its own.
- **Names are a public contract,** and the obvious one is taken: `fleetctl` is the command-line
  tool of FleetDM, an actively maintained fleet-management product for the same audience, installed
  with `npm install -g fleetctl` and from its GitHub releases. Two `fleetctl` on one `PATH` cannot
  both be found, and a search for either finds the other.
- **The release is a contract too.** It is one workflow run from one version
  ([ADR-0019](0019-the-client-supervisor-installed-service-releases-and-installers.md) clause 30),
  and every row of its build matrix already builds the packer for the runner it runs on, Windows
  included, because the Client's archives are packed by it (clause 31 there). Its assets are listed exhaustively (clause 34 there), and
  nothing in it is signed: one `SHA256SUMS` covers every asset.

## Decision

We will build the operator tools as one program, `opamp-fleetctl`, from `crates/fleet-tools`, and
publish it in every release as a `.tar.gz` for Linux and macOS on amd64 and arm64. We will give it
`pki` commands that make, with `rcgen`, the three certificate authorities, the Server's and a
Gateway's certificate and the bootstrap certificate, give it and the Server a `pki status` command
that says when the certificates each can see end, and have the running Server warn before a
certificate it depends on ends, so that no step of setting up or running a fleet needs `openssl`
or another external tool and no certificate ends unannounced.

1. **Three commands that make, and a status where the files are.** `opamp-fleetctl pki init` makes
   a fleet's certificates from nothing; `opamp-fleetctl pki server-cert` makes a server certificate
   from the existing server CA, for the Server or for a Gateway; `opamp-fleetctl pki
   bootstrap-cert` makes a bootstrap certificate from the existing bootstrap CA. `opamp-fleetctl
   pki status` says when the certificates in the offline directory end, and `server pki status`
   when those of the Server host do; the Server's runs and exits like `hash-credential` and
   `audit-verify`, before the Server starts serving. The Server binary carries no code that makes a
   CA, a server certificate or a bootstrap certificate. When a certificate counts as ending (clause 11)
   is one rule in `fleet-core`, plain date arithmetic, so both commands and the running Server judge
   alike; each program reads the certificate itself. None of the commands makes a host certificate: a host obtains its
   certificate through enrolment alone (ADR-0021).

2. **No dependency beyond ADR-0021 clause 12.** The commands use `rcgen` with the features that
   clause states, and `time`. Every key is ECDSA P-256 and written as PKCS#8 PEM, the form
   `[client_ca]` loads; every certificate is PEM; every serial is 16 random bytes, as the client
   CA's are.

3. **`init` writes two directories, and the split is the point.** `--server-dir` receives what the
   Server host needs: `server.pem` and `server-key.pem`, `client-ca.pem` and `client-ca-key.pem`,
   `bootstrap-ca.pem`, and `server.toml.fragment` naming them in `[tls]`, `[client_ca]` and
   `[enrolment]`. `--offline-dir` receives what stays with the operator: `server-ca.pem` and
   `server-ca-key.pem`, `bootstrap-ca.pem` and `bootstrap-ca-key.pem`, the bootstrap pair
   `bootstrap.pem` and `bootstrap-key.pem` that every new host is given, and
   `supervisor.toml.fragment` naming `server-ca.pem` in `[tls] ca_file` and the bootstrap pair in
   `cert_file` and `key_file`. No key of the server CA or the bootstrap CA is ever written to
   `--server-dir`. Each fragment names its files by the directory they are meant to live in:
   `--server-path` (default: the absolute path of `--server-dir`) and `--host-path` (default: the
   absolute path of `--offline-dir`). The command prints which directory goes where, and the
   bootstrap certificate's serial.

4. **`init` is meant to run where the offline keys are to stay.** That is the operator's Linux or
   macOS machine, the one `opamp-fleetctl` is downloaded to. Run on the Server host, it is followed by moving
   `--offline-dir` off it, and the command says so when both directories are on one file system.

5. **The CAs are told apart by subject and sign no CA.** `--fleet <name>` (default `opamp-fleet`;
   letters, digits, space, `.` and `-`, at most 40 characters) names the fleet; the three CAs are
   `CN=<name> server CA`, `CN=<name> client CA` and `CN=<name> bootstrap CA`, so the bootstrap CA
   never shares a subject with the client CA (ADR-0021 clause 19). Each CA certificate is
   `CA:TRUE` with a path length of 0, `keyCertSign` and `cRLSign`.

6. **A server certificate names what is dialled, and the loopback.** `--name` is given at least
   once and takes a DNS name or an IP address; an empty value, a wildcard, or a name that is
   neither is refused. Each becomes a subject alternative name, and `IP:127.0.0.1` and `IP:::1`
   are always added, so the Operator plane on its loopback default is reachable through a tunnel
   with the same certificate. The certificate is `CA:FALSE`, `digitalSignature`, `serverAuth`, and
   its subject is the first `--name`.

7. **`server-cert` serves the Server and a Gateway alike.**
   `opamp-fleetctl pki server-cert --offline-dir <dir> --name … --out <dir>` signs `server.pem` and
   `server-key.pem` in `--out` with `server-ca-key.pem`. For the Server they replace the files of
   `[tls]`, and the Server is restarted on them; for a Gateway they are its `[gateway.tls]`
   `cert_file` and `key_file`
   ([ADR-0015](0015-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)),
   which the Agents behind it verify against the same `server-ca.pem`. The fleet's trust does not
   change, so no host's configuration does.

8. **The bootstrap certificate is a client certificate and nothing more.** `CA:FALSE`,
   `digitalSignature`, `clientAuth`, its subject the bootstrap CA's with `CA` dropped (`CN=<name>
   bootstrap`). One serves every new host; what limits it is the enrolment window and the
   operator's approval (ADR-0021 clauses 20 and 21).
   `opamp-fleetctl pki bootstrap-cert --offline-dir <dir> --out <dir>` signs a new `bootstrap.pem` and
   `bootstrap-key.pem` in `--out` with `bootstrap-ca-key.pem` and prints its serial, which is what
   revokes it ([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)).

9. **Lifetimes by default, each overridable, never past the CA.** The CAs live 3650 days, a server
   certificate 397 days, the bootstrap certificate 10 days, because it carries a host only to its
   first certificate and every new host shares it; `--days` overrides the one certificate
   a command makes, and `init` takes `--ca-days`, `--server-days` and `--bootstrap-days`. A
   certificate a `pki` command signs ends no later than the CA that signs it, and one asked to
   outlive it is cut to the CA's end with a notice. Host certificates are the running Server's,
   signed for `[client_ca] validity_days` as ADR-0021 clause 9 states.

10. **Nothing is overwritten, and nothing is half written.** A command that would write a file
    that exists refuses before it writes anything, naming the file. It writes into a new
    temporary directory beside its target and renames into place only once every file is
    complete. Directories it creates are `0700`; every key is `0600`.

11. **The running Server warns before a certificate it depends on ends.** At startup and once a day
    while it runs, the Server reads the end of its own certificate (`[tls] cert_file`), of the
    client CA (`[tls] client_ca_file` and `[client_ca] cert_file`) and of the bootstrap CA
    (`[enrolment] bootstrap_ca_file`). One that ends within 30 days — or, for a certificate whose
    whole life is shorter than 90 days, within the last third of it — is *ending*: it is logged
    as a warning naming the file, its subject and the date, and recorded in the audit as
    `pki.expiring`; one that has
    ended is logged as an error and recorded as `pki.expired`
    ([ADR-0022](0022-an-append-only-audit-record-chained-by-hash.md)). Neither stops the Server:
    the hosts it serves decide for themselves, and a Server that refuses to start cannot be told
    to renew.

12. **`pki status` says when each certificate ends.** `server pki status --config <server.toml>`
    lists the files of clause 11; `opamp-fleetctl pki status --offline-dir <dir>` lists the server
    CA, the bootstrap CA and the bootstrap certificate in the offline directory. Each prints every
    certificate with its subject, its serial, its end and the days left, and exits `0` when none
    is ending in the sense of clause 11, `1` when one is, and `2` when one has ended, so a
    monitoring job can run it; a fresh 10-day bootstrap certificate is not ending.

13. **The project uses its own commands.** `scripts/dev-pki.sh` makes its development set with
    `cargo run -p fleet-tools -- pki init`, writes a complete `server.toml` and `supervisor.toml`
    from the fragments, and needs no `openssl`. Its development Client holds the bootstrap pair
    and enrols like any other host; `scripts/seed_test_configs.sh` opens the window and approves
    its request. The operator manual describes the `pki` commands, reads a serial with
    `pki status`, and names no external tool. Certificates made elsewhere keep working: the Server
    reads any PEM that meets the requirements these commands write down.

14. **Keys are stored unencrypted.** The Server reads the client CA's key and its own key
    unattended, and the offline keys are protected by where they are kept and by their mode, as
    every other key of the project is.

15. **One program, two command trees.** `opamp-fleetctl package` holds `fetch`, `pack`, `sha256`,
   `sign`, `keygen` and `public-key`, each with the options and output it has today;
   `opamp-fleetctl pki` holds `init`, `server-cert`, `bootstrap-cert` and `status` as clauses 1 to 14
   state them.
   `opamp-fleetctl --version` prints the version every binary of this project prints
   ([ADR-0012](0012-versions-resolved-in-the-internal-crate.md)). No other program name exists for
   any of these commands: no alias, no wrapper, no symlink.

16. **The crate builds exactly this program.** `crates/fleet-tools` has no library and one binary,
   `opamp-fleetctl`, and keeps the dependency direction of
   [ADR-0010](0010-five-crates-the-whole-opamp-communication-layer-in-the-opamp-crate-and-toml-configuration.md)
   clause 22: it uses the Client's and the internal crate's items, and nothing depends on it. The
   program is `src/main.rs`, which parses the command line, with one module per command tree,
   `src/package.rs` and `src/pki.rs`; the release fetcher behind `package fetch` is
   `src/fetch.rs`.

17. **Built in the release run, from the release's version, for four targets.**
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, each in the row of the Client's build matrix for the same target. Each
   row whose target runs on its runner asserts that `opamp-fleetctl --version` reports the release
   version, as the Client's build does. The Windows row builds the program for its runner only as
   the packer of the Client's archive; no Windows archive of it is published.

18. **One archive per target, named like the Client's.**
   `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, by the grammar of ADR-0019 clause 32 and the
   platform tokens of its clause 30, packed by `opamp-fleetctl package pack --format tar.gz` with the one
   member `opamp-fleetctl` and its executable mode. No `.deb`, `.rpm` or `.msi` carries it. The
   four archives are assets of the release beside the Client's and are covered by the same
   `SHA256SUMS`.

19. **It is not a package of the fleet.** The release notes list the four archives as the operator
   tool and say they are downloaded, not uploaded to the Server. Everything that selects the
   Client's archives selects them by their `supervisor_` stem, never by version and platform
   alone: the notes' upload loop, which would otherwise put an `opamp-fleetctl` archive into the
   `supervisor` Package as the entry of its platform, and `opamp-fleetctl package fetch --agent
   supervisor`. An upload made by hand anyway installs nothing: the Client's self-update refuses
   an artifact whose member is not `supervisor` (ADR-0019 clause 29).

20. **Unsigned, as the Client's artifacts are.** The archives carry what every asset of a release
   carries: their entry in `SHA256SUMS`. Provenance for the release's assets is one decision for
   all of them, not a property of this program alone.

**Out of scope:** issuing a host certificate by any route other than enrolment, which the
specification rules out; encrypting keys at rest, a hardware security module, or a passphrase
prompt; replacing a CA, which every host's configuration or enrolment follows; limiting the
running Server's signatures to its CA's end, which would change ADR-0021 clause 9; certificate
revocation lists or OCSP; ACME and certificates from a public CA, which the Server reads as they
are; a `pki` command on the Client binary; the Server's `pki status` on Windows or macOS, where
the Server does not run; the operator tool on Windows; native packages, Homebrew, `npm` or any
other installer; signing or attesting the release's assets; commands that drive the running fleet
through the Operator plane beyond the upload `package fetch` makes.

## Alternatives considered

- **Keep `openssl` and document it better.** Rejected: the rules stay in prose an operator has to
  get right by hand, and a system tool whose behaviour differs between versions stays a
  prerequisite of every fleet.
- **All the commands on the Server binary,** beside `pki status`, `hash-credential` and
  `audit-verify`. The Server binary is certain to be at hand, but it is the binary of the host the
  package signing key and the server CA's and the bootstrap CA's keys are kept away from, and the
  archive writers and the release fetcher would be linked into the binary that faces the fleet —
  attack surface with no use in serving
  ([ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)
  clause 6 has the Server never create or open an artifact). With the operator tool published, the
  one argument for it — no second binary to obtain — is gone; the version both share is the
  release's.
- **The whole reading of a certificate's end in `fleet-core`.** One function from file to verdict,
  but it would give the internal crate its first dependencies (`x509-parser`, `time`, the PEM
  reader), which every end then compiles for a rule only two programs apply; the date rule alone
  needs none.
- **`fleet-tools` depending on `fleet-server` for the rule.** No new code in `fleet-core`, but the
  operator tool would link the Server's library, its router included, for a dozen lines.
- **`pki status` in one program only.** The Server host does not hold the server CA's certificate
  and the operator's machine does not hold `server.toml`, so either program alone sees only half of
  the certificates, and the server CA's end — which every host's trust hangs on — would be visible
  nowhere.
- **Issue a host certificate from the command line**, offline with the client CA's key or through
  a route of the running Server. Rejected: the specification has a host obtain its certificate
  only through an approved, time-limited enrolment over the protocol's own means, never through a
  side channel, and a route would let the operator credential alone mint a member.
- **Limit every signature of the running Server to its CA's end, and refuse renewals under an
  ended CA.** It belongs with ADR-0021 clause 9, which states how the Server signs; kept out of
  this decision, which warns instead (clauses 11 and 12).
- **Encrypt the offline CA keys with a passphrase.** It needs an encryption crate beyond clause 2
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
  second is the build-time product name the Client's service is registered under (ADR-0019), so a
  program of that name reads as the Client.
- **All five targets, Windows included.** Windows would need its own way to keep a key readable by
  its owner alone, a piece of work this decision does not take on.
- **Linux only.** Simpler by two builds, but an operator on macOS — a likely operator machine —
  would again need a toolchain, and macOS gives the same file modes as Linux.
- **Keep the tools out of the release.** No release change at all, and the status quo: every
  operator builds them from a checkout, and a fleet cannot install a single signed package
  without a Rust toolchain on someone's machine.

## Sources / Prior art

- HashiCorp Consul, `consul tls ca create`: a self-signed CA made by the product's own binary, with
  a days option and a default common name.
  <https://developer.hashicorp.com/consul/commands/tls/ca>
- HashiCorp Nomad, `nomad tls ca create` and `nomad tls cert create`: a CA and server, client and
  CLI certificates from the product's own binary.
  <https://developer.hashicorp.com/nomad/commands/tls/ca-create>,
  <https://developer.hashicorp.com/nomad/docs/commands/tls/cert-create>
- Kubernetes, `kubeadm init phase certs ca`: the cluster's CAs generated by the setup tool itself,
  skipping files that exist. <https://kubernetes.io/docs/reference/setup-tools/kubeadm/kubeadm-init-phase>
- `rcgen`: `CertificateParams::self_signed` and `signed_by`, `BasicConstraints::Constrained`.
  <https://docs.rs/rcgen>
- `rustls-pki-types` `TrustAnchor`: a subject, a key and name constraints, no validity — why an
  expired CA goes unnoticed by this project's own ends. <https://docs.rs/rustls-pki-types>
- This project's own requirements: ADR-0021 clauses 9, 12 and 19, ADR-0013 clause 9, and
  `docs/SPECIFICATION.md` on how an Agent obtains its certificate.
- Kubernetes publishes `kubectl` per platform as a download of its own, apart from the components
  that run on a node or the control plane. <https://kubernetes.io/releases/download/>
- FleetDM's `fleetctl`: installed with `npm install -g fleetctl`, from an install script, or from
  its GitHub releases. <https://fleetdm.com/guides/fleetctl>,
  <https://www.npmjs.com/package/fleetctl>
- HashiCorp Nomad and Consul put their certificate commands into the one binary that also runs the
  servers (`nomad tls`, `consul tls`) — the arrangement this decision does not follow, because here
  the operator's keys are to stay off the server host.
  <https://developer.hashicorp.com/nomad/commands/tls/ca-create>,
  <https://developer.hashicorp.com/consul/commands/tls/ca>
- This project: ADR-0026 on why a package is
  signed by a key the operator holds, ADR-0019 clause 29 on the member a package
  artifact carries and clauses 30 to 34 on the release, its naming and its assets.

## Consequences

- Positive: a fleet is set up with two binaries the project builds and nothing else (G-17, Q-1);
  the requirements a hand-made certificate can miss are written once, in code that is tested; the
  offline keys have a place of their own from the first command on; an ending CA or server
  certificate is announced 30 days ahead in the log, the audit and a command a monitor can run.
- Positive: an operator downloads one program and can make the fleet's certificates, a signing
  key and signed packages without a toolchain (G-10, G-17, Q-1); the keys that are to stay off the
  Server host are made by a program that has no reason to be there; the Server binary carries no
  command that makes its offline keys; the packer the release uses is the one an operator
  downloads.
- Negative / trade-offs: the Server binary carries `pki status` beside serving, and the operator tool a second job beside packages; the development
  Client enrols instead of starting with a ready certificate, which adds an approval to a fresh
  development setup; keys at rest are only as safe as the place they are kept; a host certificate
  signed by the running Server can still outlive its CA, which the warnings announce rather than
  prevent.
- Negative / trade-offs: four more builds and four more assets per release; the release now
  carries a program that makes keys, and like every asset it is checked by its `SHA256SUMS` alone;
  an operator on Windows still builds from a checkout; every script that calls
  `opamp-package-fetch` or `opamp-package-sign` breaks, with no alias to soften it, which
  `CHANGELOG.md` and the notes of the release that carries the change name as ADR-0019 clause 34
  asks.
- Follow-ups: encrypting the offline keys; replacing a CA without every host enrolling again;
  limiting the running Server's signatures to its CA's end; a view of the certificates' ends in
  the bundled UI.
- Follow-ups: provenance for every release asset; the operator tool on Windows with key files
  only their owner can read; a command tree for the running fleet, if one is ever wanted, under
  this program's name.

## Enforcement

Tests that will carry `Verifies: ADR-0028`:

- `pki init` writes exactly the files of clause 3 into the two directories, no server-CA or
  bootstrap-CA key into `--server-dir`, keys `0600`, directories `0700`, fragments that name
  `--server-path` and `--host-path`, and refuses a second run over the same directories without
  writing anything (clauses 3 and 10).
- The CAs' subjects are distinct, carry a path length of 0, and pass the Server's startup check;
  a server certificate carries every `--name`, `IP:127.0.0.1` and `IP:::1`, and a wildcard or empty
  `--name` is refused (clauses 5 and 6).
- End to end, in `crates/fleet-tools/tests/`: `opamp-fleetctl pki init` makes the set, a Server
  started on `server.toml.fragment` — `fleet-server` as a development dependency of `fleet-tools`,
  which links into no artifact — admits a Client started on `supervisor.toml.fragment` through
  `fleet-agent` once its enrolment is approved (clauses 3, 6, 8 and 13).
- The Server binary refuses `pki init`, `pki server-cert` and `pki bootstrap-cert` as unknown
  commands, and its usage names `pki status` alone (clause 1).
- `server-cert` and `bootstrap-cert` sign with the offline CA; a Client that trusts the old
  `server-ca.pem` accepts the new server certificate, and the printed serial revokes the new
  bootstrap certificate (clauses 7 and 8).
- A certificate asked to outlive its CA ends with the CA (clause 9).
- A Server whose client CA ends within 30 days logs the warning and records `pki.expiring` at
  startup, and one whose CA has ended records `pki.expired` and keeps serving (clause 11).
- Both `pki status` commands, and the running Server, judge one certificate alike (clauses 1 and 11).
- Each `pki status` lists each file with its serial and end, and exits `0`, `1` and `2` for a set with
  no, an ending and an ended certificate (clause 12).
- `scripts/dev-pki.sh` runs with no `openssl` on `PATH` (clause 13).
- The command tree: `opamp-fleetctl --help` lists `package` and `pki` and nothing else beside
  `help`; `package --help` lists `fetch`, `pack`, `sha256`, `sign`, `keygen` and
  `public-key`; `pki --help`
  lists `init`, `server-cert`, `bootstrap-cert` and `status`; `--version` prints the baked version
  (clause 15).
- The crate: `crates/fleet-tools` declares exactly one binary target, `opamp-fleetctl`, and no
  library (clause 16).
- The release workflow, read as data: it builds `opamp-fleetctl` for exactly the four targets of
  clause 17, asserts its version where the target runs, names each archive
  `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, packs it with `opamp-fleetctl package pack
  --format tar.gz`, and includes it in `SHA256SUMS`; no `opamp-fleetctl_*_windows_*` asset and no
  installer names it (clauses 17 and 18).
- The release notes' upload loop, read out of the workflow, selects `supervisor_<version>_*.tar.gz`
  and matches no `opamp-fleetctl_` archive, and `package fetch --agent supervisor`, given a release
  that also holds `opamp-fleetctl_1.2.3_linux_amd64.tar.gz`, plans only the `supervisor_` archives
  (clause 19).

**Not mechanically decidable:** that no document of the project tells an operator to use an
external tool (clause 13) is held by review of every change to the manual, and that the release
notes describe the archives as the operator tool and not as packages (clause 19) by review of the
release notes in the workflow.
