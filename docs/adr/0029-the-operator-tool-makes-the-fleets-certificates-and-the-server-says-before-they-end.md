# ADR-0029: The operator tool makes the fleet's certificate authorities, the Server's certificate and the bootstrap certificate, and the Server says before any of them ends

- **Status:** 🟡 proposed
- **Date:** 2026-10-09
- **Deciders:** Markus Brigl
- **Applies to:** the `pki` commands of `opamp-fleetctl` in `crates/fleet-tools/` and the module that carries them, the Server's `pki status` command in `crates/fleet-server/src/main.rs` and the module that carries it, the ending rule in `crates/fleet-core/`, the expiry warnings of the running Server, `scripts/dev-pki.sh` and what reads its output (`scripts/seed_test_configs.sh`, `.vscode/launch.json`, `.vscode/tasks.json`, the startup message of `crates/fleet-server/src/config.rs` that names it), and every document that tells an operator how to make or inspect a certificate

## Context

An Agent proves fleet membership with a client certificate in the TLS handshake and nothing else,
and the Server presents a certificate of its own on both planes
([ADR-0022](0022-admission-by-a-client-certificate-alone.md),
[ADR-0012](0012-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)).
That takes three certificate authorities, each with one job: a server CA that signs the Server's
certificate and a Gateway's downstream certificate, the client CA the Server signs every host's
certificate with (ADR-0022 clause 9), and a bootstrap CA that signs the one certificate a new host
enrols with (ADR-0022 clause 19). Every host certificate is already made by the Server itself,
with `rcgen` (ADR-0022 clause 12), and a host obtains its first one only through an enrolment an
operator opens and approves. Everything before that is left to the operator: the three CAs, the
Server's certificate, a Gateway's certificate and the bootstrap certificate. The project's only
help is `scripts/dev-pki.sh`, which calls the `openssl` command-line tool, and the operator manual
repeats `openssl` commands for production and for reading a certificate's serial.

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
  system libraries out (ADR-0022 clause 12), and the operator's machine should not need one
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
- **The operator tool is what an operator has first.** `opamp-fleetctl` is published with every
  release for Linux and macOS and runs on the operator's own machine
  ([ADR-0030](0030-the-operator-tools-are-one-program-released-for-linux-and-macos.md)), before any
  Server exists; the Server binary runs on the Server host, where two of the three CA keys are not
  to be.

## Decision

We will give the operator tool `opamp-fleetctl` `pki` commands that make, with `rcgen`, the three
certificate authorities, the Server's and a Gateway's certificate and the bootstrap certificate,
give both programs a `pki status` command that says when the certificates each can see end, and
have the running Server warn before a certificate it depends on
ends, so that no step of setting up or running a fleet needs `openssl` or another external tool
and no certificate ends unannounced.

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
   certificate through enrolment alone (ADR-0022).

2. **No dependency beyond ADR-0022 clause 12.** The commands use `rcgen` with the features that
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
   never shares a subject with the client CA (ADR-0022 clause 19). Each CA certificate is
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
   ([ADR-0014](0014-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)),
   which the Agents behind it verify against the same `server-ca.pem`. The fleet's trust does not
   change, so no host's configuration does.

8. **The bootstrap certificate is a client certificate and nothing more.** `CA:FALSE`,
   `digitalSignature`, `clientAuth`, its subject the bootstrap CA's with `CA` dropped (`CN=<name>
   bootstrap`). One serves every new host; what limits it is the enrolment window and the
   operator's approval (ADR-0022 clauses 20 and 21).
   `opamp-fleetctl pki bootstrap-cert --offline-dir <dir> --out <dir>` signs a new `bootstrap.pem` and
   `bootstrap-key.pem` in `--out` with `bootstrap-ca-key.pem` and prints its serial, which is what
   revokes it ([ADR-0023](0023-certificate-revocation-that-follows-renewal-and-reaches-the-gateways.md)).

9. **Lifetimes by default, each overridable, never past the CA.** The CAs live 3650 days, a server
   certificate 397 days, the bootstrap certificate 30 days; `--days` overrides the one certificate
   a command makes, and `init` takes `--ca-days`, `--server-days` and `--bootstrap-days`. A
   certificate a `pki` command signs ends no later than the CA that signs it, and one asked to
   outlive it is cut to the CA's end with a notice. Host certificates are the running Server's,
   signed for `[client_ca] validity_days` as ADR-0022 clause 9 states.

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
    ([ADR-0024](0024-an-append-only-audit-record-chained-by-hash.md)). Neither stops the Server:
    the hosts it serves decide for themselves, and a Server that refuses to start cannot be told
    to renew.

12. **`pki status` says when each certificate ends.** `server pki status --config <server.toml>`
    lists the files of clause 11; `opamp-fleetctl pki status --offline-dir <dir>` lists the server
    CA, the bootstrap CA and the bootstrap certificate in the offline directory. Each prints every
    certificate with its subject, its serial, its end and the days left, and exits `0` when none
    is ending in the sense of clause 11, `1` when one is, and `2` when one has ended, so a
    monitoring job can run it; a fresh 30-day bootstrap certificate is not ending.

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

**Out of scope:** issuing a host certificate by any route other than enrolment, which the
specification rules out; encrypting keys at rest, a hardware security module, or a passphrase
prompt; replacing a CA, which every host's configuration or enrolment follows; limiting the
running Server's signatures to its CA's end, which would change ADR-0022 clause 9; certificate
revocation lists or OCSP; ACME and certificates from a public CA, which the Server reads as they
are; a `pki` command on the Client binary; the operator tool on Windows, where it
is not published (ADR-0030); the Server's `pki status` on Windows or macOS, where the Server does not run.

## Alternatives considered

- **Keep `openssl` and document it better.** Rejected: the rules stay in prose an operator has to
  get right by hand, and a system tool whose behaviour differs between versions stays a
  prerequisite of every fleet.
- **All four commands on the Server binary.** The Server binary is certain to be at hand, but it
  is the binary of the host the server CA's and the bootstrap CA's keys are kept away from, and it
  would carry code no running Server uses. With the operator tool published (ADR-0030), the one
  argument for it — no second binary to obtain — is gone; the version both share is the release's.
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
- **A `pki` command on the Client binary.** The Client is the one program a release ships, but it
  lands on every host, and every host would then carry the means to make a CA.
- **Issue a host certificate from the command line**, offline with the client CA's key or through
  a route of the running Server. Rejected: the specification has a host obtain its certificate
  only through an approved, time-limited enrolment over the protocol's own means, never through a
  side channel, and a route would let the operator credential alone mint a member.
- **Limit every signature of the running Server to its CA's end, and refuse renewals under an
  ended CA.** It belongs with ADR-0022 clause 9, which states how the Server signs; kept out of
  this decision, which warns instead (clauses 11 and 12).
- **Encrypt the offline CA keys with a passphrase.** It needs an encryption crate beyond clause 2
  and a prompt the commands must handle; kept out of scope, not ruled out.
- **Recommend `step` or `cfssl`.** Either makes certificates well, but each is one more tool to
  install and to learn, and neither knows this project's requirements.

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
- This project's own requirements: ADR-0022 clauses 9, 12 and 19, ADR-0012 clause 9, and
  `docs/SPECIFICATION.md` on how an Agent obtains its certificate.

## Consequences

- Positive: a fleet is set up with two binaries the project builds and nothing else (G-17, Q-1);
  the requirements a hand-made certificate can miss are written once, in code that is tested; the
  offline keys have a place of their own from the first command on; an ending CA or server
  certificate is announced 30 days ahead in the log, the audit and a command a monitor can run.
- Negative / trade-offs: the Server binary carries `pki status` beside serving, and the operator tool a second job beside packages; the development
  Client enrols instead of starting with a ready certificate, which adds an approval to a fresh
  development setup; keys at rest are only as safe as the place they are kept; a host certificate
  signed by the running Server can still outlive its CA, which the warnings announce rather than
  prevent.
- Follow-ups: encrypting the offline keys; replacing a CA without every host enrolling again;
  limiting the running Server's signatures to its CA's end; a view of the certificates' ends in
  the bundled UI.

## Enforcement

Tests that will carry `Verifies: ADR-0029`:

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

**Not mechanically decidable:** that no document of the project tells an operator to use an
external tool (clause 13) is held by review of every change to the manual.
