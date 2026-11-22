# CLT-icinga2-repacked-kind: Icinga 2 runs as the `icinga2` kind from a repacked vendor tree, enrols with its Icinga master, and reaches the hosts whose glibc is at least its Debian suite's

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/icinga2.rs, the `icinga2` feature of crates/fleet-agent/Cargo.toml, the kind's use of the preflight, version probe and its environment, version parser, process-group stop and awaiting-configuration health of crates/fleet-agent/src/supervisor/process.rs, the Icinga tests in crates/fleet-agent/src/reconfigure.rs and crates/fleet-agent/src/supervisor/mod.rs, crates/fleet-agent/src/bin/stub_icinga2.rs, crates/fleet-agent/tests/icinga2_supervisor.rs, `icinga2_plans`, the Debian index and the repack in crates/fleet-tools/src/fetch.rs, crates/fleet-tools/src/fetch/debian.rs, the trust anchors under crates/fleet-tools/trust/, the fixtures crates/fleet-tools/testdata/icinga-bookworm-InRelease, crates/fleet-tools/testdata/debian-bookworm-security-InRelease and crates/fleet-tools/testdata/debian-bullseye-security-InRelease, fuzz/fuzz_targets/deb_release.rs, fuzz/fuzz_targets/deb_index.rs, fuzz/fuzz_targets/deb_alternatives.rs and fuzz/fuzz_targets/elf_needed.rs with their seeds under fuzz/seeds/, config/examples/icinga2-*.conf, docs/artifacts/icinga2.md, docs/manual/icinga2.md

## Context

Icinga 2 should reach a host the way every other Managed Process does: a package the Server
offers and the Client unpacks, updates and rolls back, never a distribution package installed
beside the fleet. The target is the Icinga **Agent** role, which needs a certificate signed by an
Icinga master. A kind knows its own agent
([CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md)), and Icinga 2 is the agent where
a kind is not merely shorter than a recipe but required. A spike against Icinga 2.14.6 measured why.
A repacked tree runs from any directory — `strace` showed no compiled-in path touched — but only
under conditions no block can carry:

- **Every invocation must name its account.** Without `-D RunAsUser=`/`-D RunAsGroup=` every
  subcommand — `daemon`, `pki`, a validation — refuses (*"Please re-run this command as a privileged
  user or using the `nagios` account"*): the compiled-in user does not exist on a fleet host.
- **The ITL is found through `-D IncludeConfDir=`, not `-I`.** With `-I` alone, `include <itl>`
  silently resolved to the host's `/usr/share/icinga2`.
- **Icinga creates none of its directories**; startup fails on the first write. Debian's
  `prepare-dirs` hard-fails without the `nagios` user.
- **A failed reload is silent.** After `SIGHUP` with a broken configuration the daemon logs
  *"Found error in config: reloading aborted"* to stderr and keeps running the old one, same pid.
- **A killed umbrella orphans its worker.** `SIGTERM` takes the worker along within two seconds;
  `SIGKILL` leaves it running, reparented to init, holding the data directory and port 5665.

`SIGHUP` keeps the umbrella's pid, so the reload fits the shared Runner as it is; `--version`
prints `r2.14.6-1`, which a strict SemVer read rejects.

**Enrolment.** The Icinga flow is ticket-based and non-interactive: the master computes a ticket as
an HMAC of the node's common name under its `TicketSalt`; the node generates its key locally, sends
a CSR, and receives a signed certificate. `icinga2 node setup` does it in one command but writes
into `ConfigDir`, the one constant not reliably relocatable; the `pki` subcommands (`new-cert`,
`save-cert`, `request`, `verify`) take every path as an argument, touch no system directory, and
write the key `0600`.

**The artifact.** Icinga publishes `.deb`/`.rpm` packages and an MSI — no portable tree. The real
binary carries `RUNPATH`, not `RPATH`, so `LD_LIBRARY_PATH` wins without `patchelf`. Its closure
outside glibc is about twenty shared objects (Boost, OpenSSL, `libsystemd`, `libedit`, `libstdc++`,
and, from 2.16, `boost_regex` with ICU). glibc cannot be bundled — and glibc is backward compatible,
so a tree's reach is one number: the highest glibc any of its files needs. Each Debian package
states its own as `Depends: libc6 (>= …)`, and a bundled library can need a newer one than the
vendor's binary: bookworm's `libstdc++6` states 2.36 where `icinga2-bin` states 2.34, and
`objdump -T` finds `GLIBC_2.36` in it. Measured for Icinga 2 2.16.5:

| Suite repacked | `icinga2-bin` states | The tree needs | Runs on |
|---|---|---|---|
| Debian 11 (bullseye) | 2.30 | 2.30 | Debian 11+, Ubuntu 20.04+, RHEL 9+ |
| Debian 12 (bookworm) | 2.34 | 2.36 | Debian 12+, Ubuntu 24.04+, RHEL 10+ — not Ubuntu 22.04 (2.35), not RHEL 9 (2.34) |
| Debian 13 (trixie) | 2.38 | 2.39 | Debian 13+, Ubuntu 24.04+, RHEL 10+ |

Every one of those libraries is a package of Debian's own archive, in the suite the vendor built
against. Debian clear-signs each suite's `InRelease` with several keys at once, among them the
automatic signing keys `ftp-master.debian.org/keys.html` lists per release; a signature by a
subkey names its primary key last on `gpgv`'s `VALIDSIG` line. The `-security` suites state a
`Valid-Until` a week ahead, the main suites none. Debian 11's security suite has ended: its index
still names packages whose pool files `deb.debian.org` answers `404` for, and `archive.debian.org`,
where Debian keeps ended releases, serves them under the same paths. The suite's last `InRelease`
is the same file at both, signed by the same keys, and states no `Valid-Until`; the archived main
suites state none either.

Icinga's open RPM repository ends at EL 8; from RHEL 9 on it is behind a subscription. The Windows
MSIs at `packages.icinga.com/windows/` publish no digest sidecar, and whether a tree unpacked from
one runs without the MSI's product registration has not been tried on a Windows host. Icinga's
Debian repositories publish a clear-signed `InRelease` per suite, signed with
`Icinga GmbH (Build server)`, fingerprint `DD3A F619 8ED0 00B4 C0B7 3956 CC11 6F55 AA7F 2382`;
it states the SHA-256 and size of each `Packages` index, which states those of each package. A
ticket-less `pki request` that the parent queues is answered *"is pending"* with exit status 0,
after the CA was written; the parent files the request under the presented certificate's
fingerprint and hands the signed certificate to the next request that presents the same one.

**What a delivered block can reach.** The Server delivers the `[[supervisor]]` blocks
([CLT-supervisor-mode-and-kinds §26](CLT-supervisor-mode-and-kinds.md)), and the enrolment keys are
block keys. The kind sends the content of `ticket_file` to the parent the block names, so a block
free to name any path would send `/etc/shadow` to a parent an attacker runs
([`HARDENING.md`](../HARDENING.md)). For a Supervisor whose
remote configuration the operator switched off
([CLT-supervisor-mode-and-kinds §45–§52](CLT-supervisor-mode-and-kinds.md)), a delivered block may
point `trusted_cert_file` at a file inside its own `config_dir`
([CLT-supervisor-mode-and-kinds §36](CLT-supervisor-mode-and-kinds.md)), but no remote configuration
writes that file, so it does not exist. A missing pin that fell back to `pki save-cert`, trust on
first use, would let a delivered block that changes the parent and points the pin at such a file
enrol the operator's agent with a parent the Server chose, trusting whatever certificate it
presents.

**What reaches one Agent.** A Configuration has no Selector, a Deployment aims its
Configurations, and a rollout skips an Agent that two Deployments claim
([SRV-deployments-and-pinned-rollouts §2, §3, §5 and §9](SRV-deployments-and-pinned-rollouts.md)). A
Configuration meant for one host alone therefore reaches it only through a Deployment that claims
that host alone, and that Deployment then carries everything the host runs.

## Decision

We will run Icinga 2 in the Agent role as a compiled-in `icinga2` kind:

- The kind derives its whole command line, and gates starts and applies on what Icinga needs.
- It enrols with the Icinga master over the `pki` subcommands.
- It runs a link-free tree that `opamp-fleetctl package fetch` repacks out of the vendor's packages
  and Debian's library packages.
- The tool verifies each package from its signed archive. The tree bundles everything but glibc
  and takes nothing from the host that repacks it.
- The Debian suite repacked is the artifact's reach.

1. **A kind of its own, reusing the shared Runner.**
   - The kind is one module in `crates/fleet-agent/src/supervisor/` and one registry line.
   - Spawn, watchdog, backoff, bounded stop, package swap, rollback, retention and health are the
     shared `Runner`'s.
   - The kind supervises the **Agent role, never a master**, and runs the delivered tree only. Its
     program is `icinga2` plus `EXE_SUFFIX` at `program_path` `sbin/icinga2[.exe]`, with
     `service_name = "icinga2"`.
   - It refuses an `endpoint_port`: Icinga speaks its cluster protocol to its parent, not OpAMP to
     us.
   - For the same reason the daemon gets no Supervisor Endpoint token.
     [CLT-modes-and-gateway §28](CLT-modes-and-gateway.md) lets a kind decline it whose program
     never connects to its Supervisor Endpoint. The token would serve no connection and would reach
     every check plugin the daemon runs.

2. **The block holds the enrolment, and nothing else.** Four optional keys, parsed strictly:

   | Key | What the Agent does with it |
   |---|---|
   | `node_name` | its `NodeName`, the CN of its certificate, its Endpoint name |
   | `parent_host` | the parent it enrols with and connects to, `host` or `host:port` (port defaults to 5665; a bracketed IPv6 address may carry a port, a bare one never does) |
   | `ticket_file` | the file holding the ticket it presents when asking for a certificate |
   | `trusted_cert_file` | the parent's own certificate, pinned instead of trusted on sight |

   - No `parent_host` means a standalone Icinga instance: no enrolment, local checks only.
   - All four keys are rollable. The `[[supervisor]]` blocks are the fleet-managed half of the
     Client's configuration ([CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md)).
   - The two files travel as Configurations (clause 11).

3. **`node_name` defaults to the host's FQDN.**
   - The kind resolves it with `getaddrinfo` and `AI_CANONNAME`, the route `hostname --fqdn`
     takes. It resolves it once per process, and only when no `node_name` is set.
   - It accepts the result only as a DNS host name that contains a dot. Its labels are letters,
     digits and inner hyphens.
   - Failing that, the default is the Supervisor's name. On Windows nothing is resolved.
   - The default is not `host.name`. That is `gethostname`, usually the short name, and a default
     that fails enrolment.
   - The key stays, because a host whose master knows it under another CN must be able to say so.
     A mismatch fails enrolment.

4. **Every other key is the kind's, and refused by name.**
   - A block that carries one of these keys fails at startup, naming what supplies the value:
     `binary`, `program_path`, `service_name`, `include_dir`, `plugin_dir`, `data_dir`,
     `log_dir`, `cache_dir`, `spool_dir`, `run_dir`, `log_level`, `renew_before_days`,
     `parent_port`, `run_as_user`, `run_as_group`, `args`, `main_config`, `env`.
   - An offered Supervisor set that carries one fails `Plugin::check` the same way.
   - Logging verbosity belongs in Icinga's own `object FileLogger` `severity`, which the fleet rolls
     out.

5. **The kind derives the command line.** The daemon runs as `daemon -c <root> -D RunAsUser=…
   -D RunAsGroup=… -D NodeName=… -D IncludeConfDir=<tree>/share/icinga2/include
   -D PluginDir=<tree>/plugins -D DataDir=… -D LogDir=… -D CacheDir=… -D SpoolDir=…
   -D InitRunDir=… -x information`. Here `<tree>` is `${supervisor_dir}/program/tree`.
   - The account is the one the Client runs as, `id -un` and `id -gn` on Unix. The daemon names
     it, and so does every subcommand of clauses 9, 12 and 13.
   - The version probe of clause 16 and the preflight of clause 15 run `--version` and name no
     account.
   - Windows Icinga drops no privileges, so nothing is named there.
   - On Windows `PluginDir` is `<tree>/sbin`: the check plugins sit beside the daemon and share its
     DLLs.
   - The state directories are `${supervisor_dir}/data`, `log`, `cache`, `spool` and `run`.
   - On Unix every invocation runs with `LD_LIBRARY_PATH=<tree>/lib`: the daemon, each subcommand
     of clauses 9, 12 and 13, and the version probe.
   - The version probe is handed the start's environment, as
     [CLT-supervisor-mode-and-kinds §57](CLT-supervisor-mode-and-kinds.md) lets a kind hand it.
     The preflight runs with the staged tree's. One rule derives all of them.
   - Windows needs no environment.
   - The daemon runs in the foreground: no `-d`, no `--close-stdio`. Its stdout and stderr go into
     the Client's logging.

6. **The kind prepares Icinga's directories.**
   - Before every spawn and before enrolment it creates `data/`, `data/certs/`, `log/`, `cache/`,
     `spool/` and `run/` as private directories. So one removed under a running fleet comes back.
   - It runs no `prepare-dirs`, creates no users, and drives no service manager.
   - State lives **beside** the tree. `data/` with the certificates, the enrolment marker
     `icinga2-enrolment.json` and the pinned `trusted-parent.crt` are siblings of `program/` and
     `config/`.
   - So a package swap never touches the identity, and purging the Supervisor's directory takes
     all of it.

7. **The root Configuration is marked by `role = "main"`.**
   - The daemon is pointed at one file, from which it `include`s the rest. That root cannot be
     derived from unroled entries.
   - [SRV-configurations-compose-one-map §5](SRV-configurations-compose-one-map.md) lets a kind
     give a role a meaning of its own. `main` is the `icinga2` kind's own value, and no other kind
     reads it.
   - The kind configures its process from the entry roled `main`. That is its exception to
     [SRV-configurations-compose-one-map §6](SRV-configurations-compose-one-map.md), which leaves
     every roled entry out.
   - The kind configures its process from no other entry, except the stand-in that follows.
   - Where no entry carries `main`, the entry named `icinga2-conf` is the root.
     `opamp-fleetctl package fetch` uploads that one.
   - Two entries that carry `main` are a reason not to start, naming both.
   - The kind resolves the root on every spawn.

8. **No process until it can do its job.** The kind's `build()` yields no process while one of
   these holds:
   - the root Configuration is missing;
   - a parent is configured, and `data/certs/` lacks `<NodeName>.crt` or `ca.crt`;
   - a parent is configured, and the enrolment marker does not record this `NodeName` with this
     parent's host and port;
   - the delivered configuration is the one Icinga refused (clause 9).

   The marker keeps the daemon from starting on an earlier parent's certificate. Enrolment would
   replace that key and certificate underneath it. The Runner reports what it waits for instead of
   crash-looping toward a hold. Its health names the gate's reason.

9. **A Configuration is validated before it is applied.**
   - `daemon -C` runs against the delivered configuration with the same command line.
   - Where it ran and exited with a status of its own, Icinga refused the configuration. The kind
     then leaves the running daemon alone: not stopped, not reloaded. It answers the apply
     `ConfigApplied{Err}` with the validator's message.
   - The refused configuration stays on disk where the fleet wrote it. So the kind records it by a
     digest of the entries, with the reason. It does not start it at a later spawn either.
   - A delivery that changes the configuration ends the refusal. A validated apply clears it.
   - A `daemon -C` that did not run to an answer refused nothing. That is one that failed to
     start, that the dynamic loader failed to load (exit status 127), that a signal ended, or that
     timed out.
   - The kind then answers the apply `ConfigApplied{Err}` with that reason, leaves the running
     daemon alone, and records no refusal.
   - On success the apply goes to the Runner. On Unix that is `SIGHUP`, since the umbrella's pid is
     stable. On Windows it is a restart.
   - Icinga's slow drain makes `stop_timeout` 60 s and `apply_grace` 30 s the kind's timing.

10. **The daemon is stopped as a group.**
    - Icinga's umbrella process spawns a worker, which holds the data directory and port 5665. A
      `SIGKILL` to the umbrella alone leaves the worker running, reparented to init.
    - So the kind leads its own process group
      ([CLT-supervisor-mode-and-kinds §53](CLT-supervisor-mode-and-kinds.md)). The Runner's stop
      signals the umbrella and its worker together.
    - The escalation to `SIGKILL` cannot leave the worker behind.
    - The reload signal of clause 9 goes to the umbrella's pid alone. The umbrella handles the
      reload itself.

11. **The Icinga master is the only CA; the fleet transports, never signs.**
    - The fleet Server holds no `TicketSalt`, signs nothing, and never sees a private key.
    - The OpAMP PKI of
      [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md) and Icinga's
      never touch.
    - The fleet carries two artefacts that are not keys. Each is a Configuration with
      `role = "supplementary"`. Each lands as a file the Supervisor reads, and the daemon is never
      pointed at either:
      - the **ticket**, useless for any other CN;
      - the **parent's certificate**, which is public.
    - **The ticket travels in a Deployment per host.** Its Selector matches that one Agent.
    - While the Agent enrols, that Deployment holds everything the Agent runs
      ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)). That is the
      Package, the root and its included Configurations, and the parent's certificate besides the
      ticket.
    - Once enrolled, the Agent may move to a Deployment the fleet shares. Its rollout leaves the
      ticket behind. The certificate on disk is what counts from then on (clause 13).
    - **The parent's certificate travels wherever the Agent is deployed**, for as long as the block
      names it. A renewal pins the parent again (clause 12).

12. **Enrolment is `pki`, never `node setup`, and runs in the adapter.** A task beside the Runner
    runs it, never `Plugin::start`, so an unreachable master cannot hold up the Client's startup.
    Each subcommand is bounded at 30 s.
    - **The key.** `pki new-cert` runs with the CN and explicit key and certificate paths under
      `data/certs/`. The key never leaves the host.
    - **The pin.**
      - The kind **copies** the delivered `trusted_cert_file` to `trusted-parent.crt`, because the
        next apply empties the entry directory.
      - A named `trusted_cert_file` that is missing is a wait, for a first enrolment and a renewal
        alike.
      - Only where no `trusted_cert_file` is named does `pki save-cert` pin the parent. That is
        trust on first use, and the kind logs it as such.
      - It happens once per parent. The kind keeps the pin with the `host:port` it was taken from,
        and a renewal or retry against that parent keeps it.
    - **The request.** `pki request` asks the parent with the pinned certificate, and with
      `--ticket` when a ticket was delivered.
      - A named `ticket_file` that is not there or holds nothing is a wait. The kind reads it
        before any key is made.
      - A renewal needs no ticket.
      - Without a ticket the request waits in the master's queue for `icinga2 ca sign`. The Agent
        stays unhealthy, saying so.
      - A *"pending"* answer is a wait, not a signature. The next attempt presents the same key and
        certificate, so the certificate signed there is the one fetched.
      - A key and certificate with no CA beside them are a request in flight.
      - The kind writes the CA to a side path. It keeps it as `ca.crt` only with a signed
        certificate, so the gate of clause 8 stays shut meanwhile.
      - An enrolment that starts over removes the CA of the certificate it replaces.
    - **The ticket is a secret.**
      - A failed subcommand is reported with `--ticket`'s value replaced. The value is also
        removed from what Icinga printed. So no log line, health or fleet view carries it.
      - Icinga takes the ticket on its command line alone. While `pki request` runs, at most the
        30 s of its bound, the value stands in `/proc/<pid>/cmdline` for any local account to read.
      - The ticket is good for this CN alone.

    On success the kind writes the marker and sends the Runner a restart. That reopens the gate of
    clause 8.

13. **The certificate on disk is the state; the marker is a hint.**
    - The kind runs `pki verify` where a certificate, a CA and a marker name the same CN, parent
      host and port.
    - A certificate valid beyond 30 days needs nothing.
    - Within 30 days of its expiry, or with an expiry the kind cannot read, it is **renewed**. The
      key is kept: no `new-cert`, no ticket. The certificate in force authenticates its own
      renewal.
    - A certificate that `pki verify` ran and refused enrols again. So does a changed CN or parent.
    - A `pki verify` that did not run to an answer, as clause 9 tells one, is a wait (clause 14).
      The kind changes nothing on disk for it.
    - Renewal is otherwise the daemon's own. This is a start-time safety net.

14. **An unreachable master is a wait.** A failed attempt backs off and retries.
    - Without a certificate in force for this `NodeName` and parent, the gate of clause 8 is
      shut. The health reads `awaiting the certificate for <node>` with the reason, and no
      daemon is started.
    - With a certificate in force, the daemon keeps running on it. A verification that did not run
      or a renewal that waits for its parent is reported as such, with the reason.
    - That health names the certificate whose verification or renewal failed. It stays healthy
      while the daemon is ready to run.
    - Revocation stays on the master. Removing the Supervisor removes the key material with its
      directory, and `icinga2 ca remove` on the master is the operator's.

15. **Icinga's preflight is `--version` with `LD_LIBRARY_PATH=${staged}/lib`.**
    - A package is proved to run before it is installed
      ([CLT-supervisor-mode-and-kinds §54](CLT-supervisor-mode-and-kinds.md)).
    - The staged tree is checked against the libraries it carries. So a tree built against a newer
      glibc than the host's is refused with the dynamic linker's message.
    - That happens before the running daemon is stopped.
    - `${staged}` resolves to the staged tree's root.
    - Neither the preflight nor the version probe names a working directory, since the daemon's
      start names none ([CLT-supervisor-mode-and-kinds §57](CLT-supervisor-mode-and-kinds.md)).

16. **Icinga's version parser reads `r2.14.6-1` as `2.14.6`.**
    - The kind's version probe brings it
      ([CLT-supervisor-mode-and-kinds §55](CLT-supervisor-mode-and-kinds.md)). The strict SemVer
      default reads no version out of Icinga's banner.
    - The number it finds is written as SemVer by the rule the Package is named by
      ([PKG-versions-high-water-mark-floor §6](PKG-versions-high-water-mark-floor.md)). So the two
      agree.

17. **The artifact is the vendor's packages, repacked into one normalised, link-free tree.**
    `opamp-fleetctl package fetch --agent icinga2` builds it under the wrapper `icinga2-<version>`.
    Here `<version>` is the Package version, a later repack's included:

    ```
    sbin/icinga2            the daemon
    lib/                    the gathered libraries
    share/icinga2/include/  the ITL
    plugins/                the check plugins
    doc/                    the copyright file of every package the tree is made of
    ```

    - Left out: `/etc/icinga2`, since the fleet delivers configuration; the systemd unit and init
      script; `prepare-dirs` and `safe-reload`; documentation beyond copyright.
    - Repacking is redistribution. So the copyright file of every package the repack downloads
      travels, as `doc/<package>.copyright`.
    - Which library packages the tree carries files of is known only once the libraries are
      gathered. A copyright file too many errs toward attribution.
    - The Linux daemon is taken from `usr/lib`, not the `/usr/sbin` shell wrapper.
    - The container is **`.tar.gz`**, which carries the executable bit.
    - Packing is the deterministic repack of
      [OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md), with its normalized
      file modes.
    - The artifact is uploaded as the Package of Agent type `icinga2`, one entry per platform. Its
      version is the Icinga version written as SemVer
      ([PKG-versions-high-water-mark-floor §6](PKG-versions-high-water-mark-floor.md)).
    - A later repack of the same version is numbered by clause 27. Its wrapper and its file are
      named by that number, so it does not overwrite an earlier repack's file.

18. **Linux: every file comes from a signed archive, and nothing from the host.**
    - **The sources.**
      - `icinga2-bin` and `icinga2-common` come from the `icinga-<distro>` suite of
        `packages.icinga.com/debian`.
      - `monitoring-plugins-basic` and `-common` come from `deb.debian.org`, and so do the library
        packages below.
      - Each is taken at the newest version its suite states, in Debian's version order.
        `icinga2-common` is taken at exactly `icinga2-bin`'s version.
      - Debian's packages are read from the suite, its `-updates` and its `-security` together.
        The newest across them wins, as an `apt` with Debian's default sources installs them.
    - **Who signs.** [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) asks a
      kind to state who signs, with which key, from which version on. Here:
      - Icinga signs the index of every `icinga-<distro>` suite with its archive key,
        `crates/fleet-tools/trust/icinga-archive.gpg`, fingerprint in Context.
      - That signature covers every Icinga version the suite lists, so every version the tool
        repacks is signed.
      - Debian signs every suite with the automatic signing keys of its release. The tool carries
        those of Debian 11, 12 and 13 in `crates/fleet-tools/trust/debian-archive.gpg`.
      - The tool repacks no Debian release before 11.
      - The three archive keys verify a suite and its `-updates`. The three security archive keys
        verify its `-security`.
      - An index without a signature by a pinned primary refuses the repack. There is no unsigned
        path.
      - The pinned primaries of each archive are its keyring's own, held equal by a test.
    - **The index.**
      - A suite's `Packages` index is read only once its clear-signed `InRelease` verified as
        [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) verifies an
        upstream's OpenPGP signature.
      - Only the text the signature covers is read.
      - The signed text must name the suite it was fetched for, as its `Codename` or its `Suite`,
        as `apt` requires.
      - A text that names another suite is refused, since a signature covers the text and not the
        path it was served under.
      - A signed text whose `Valid-Until` has passed is refused, as `apt` refuses it. A text that
        states no `Valid-Until` is not bounded by one.
      - Both rules together keep an old index served again from bringing back a library a security
        update replaced. That holds whether it is served for its own suite or for another.
      - The index is `.xz` where the signed text states one, and `.gz` otherwise. It must match
        the SHA-256 and sizes that text states, compressed and uncompressed.
      - Decompression stops at the stated size, bounded at 512 MiB.
    - **Debian's archive.**
      - Where `deb.debian.org` answers `404` for a file of a suite or of its pool, the tool reads
        the same path below `archive.debian.org`.
      - `/debian` serves the suite and its `-updates`, and `/debian-security` its `-security`. So
        a suite Debian moved there still repacks.
      - Only a `404` moves the read, and nothing else about it changes. The same keyring, pinned
        primaries, `InRelease`, suite, index and `SHA256` checks apply.
      - The same `Valid-Until` rule applies. An archived suite's last `InRelease` states no
        `Valid-Until` and is not bounded by one.
      - That keeps the rule's purpose. A live `-security` suite states a `Valid-Until` a week
        ahead. So every older security index served again has passed its own and is refused.
      - The one that states none is the last of an ended release. It names that release and is
        refused for any other.
      - A main suite never states one, wherever it is read.
    - **The packages.**
      - Each is verified against the `SHA256` its index states before anything is unpacked.
      - Each is unpacked with `dpkg-deb -x`, which runs none of its scripts. The four of the tree
        go together into one directory, the library packages together into another.
      - A package unpacked later may replace a file of one unpacked earlier. Every one of them
        matched its signed digest.
      - Before each unpack, a link in that directory that leads out of it to a directory of the
        host fails the repack, naming it. `dpkg-deb -x` would write the next payload through it.
      - A link that stays inside the directory may redirect a later package's payload inside it.
        That payload matched its signed digest too.
      - An alternative a maintainer script registers is applied as `update-alternatives` applies
        it in automatic mode: the highest priority wins.
      - The alternative is written only where nothing is at that path, and through no link.
    - **The library packages** are the `Depends` and `Pre-Depends` closure of the four packages in
      the verified indexes.
      - The closure follows Debian's `libs` section alone, and leaves `libc6` out.
      - A virtual name is answered by the first package by name that `Provides` it.
      - The closure decides only what is downloaded.
    - **The libraries** are the `DT_NEEDED` closure of the daemon and of every plugin.
      - They are read from the ELF files themselves, never by running them. The closure follows
        each library gathered.
      - glibc's own sonames and its loader are left out.
      - Each library is taken out of the unpacked library packages and copied dereferenced into
        `lib/`. No `patchelf` runs.
      - A soname none of the packages holds fails the repack, naming it and the program that needs
        it. A tree missing one would ship and die on its first start.
    - **A copy out of the unpacked packages** is resolved inside the directory they were unpacked
      into together.
      - So a link in one package may name a file of another, as `libstdc++6`'s documentation names
        `gcc-12-base`'s.
      - A link that leads out of that directory fails the repack, naming it
        ([OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md)).
      - A link to a directory the copy is already inside is copied as that name and not descended
        into, as [OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md) packs it.
      - A copy holds no more files and directories than the member cap of a tree package, so links
        to directories cannot multiply it.
    - **The reach.** The tool prints the library packages and the highest `libc6` floor any package
      of the tree states. It prints both before any package is downloaded. That floor is the
      artifact's reach.

19. **The suite is the reach, and there is one artifact per platform.**
    - The tool repacks the Debian suite `--distro` names, on any host. It is one of `bullseye`,
      `bookworm` and `trixie`.
    - Omitted, it is the host's own (`VERSION_CODENAME` in `/etc/os-release`) where the host is one
      of them. Otherwise no Linux artifact is offered.
    - Repack the oldest suite the fleet serves. The artifact serves every newer system, across
      distribution families.
    - A Debian-built tree carries Debian's OpenSSL directory layout. That is inert for cluster TLS,
      whose certificates are named by explicit paths.
    - The manual names the layout as the thing to verify for checks that use the system trust
      store on Red Hat hosts.
    - The RPM repack is not built.

20. **Every download of the repack is read within a cap, and every parser of what it downloads is
    fuzzed** (specification Q-2).
    - A suite's `InRelease` is read within 4 MiB, before anything states its size. The largest the
      tool reads, Debian 12's main suite, is under 200 KB.
    - A compressed index is read within the size the signed text states for it. That size is at
      most 512 MiB.
    - A package is read within the size its verified index states as `Size:`. A package whose index
      states none refuses the repack.
    - The parsers of what the repack downloads live in `crates/fleet-tools/src/fetch/debian.rs`.
      That module names nothing of the crate (docs/CONVENTIONS.md).
    - They read the signed `Release` text, the `Packages` index, a maintainer script and an ELF
      file. Each of the four has a fuzz target: `deb_release`, `deb_index`, `deb_alternatives` and
      `elf_needed`.
    - Each target is a `[[bin]]` of `fuzz/Cargo.toml`, an entry of the matrix in
      `.github/workflows/fuzz.yml`, and a directory under `fuzz/seeds/`.

21. **Extraction and verification helpers are shelled out to, and refused by name when missing.**
    - They are `dpkg-deb`, `xz` and `gpgv`.
    - The tool never installs anything on its host.
    - No crate is added for the signature format, nor for `.xz`.

22. **No Windows artifact is built until a Windows host proves one.**
    - Nobody has tried on a Windows host whether a tree unpacked from Icinga's MSI runs without the
      MSI's product registration.
    - The kind has no other form to fall back on.
    - Until a run on a Windows host proves it, `opamp-fleetctl package fetch` builds no Windows
      artifact for Icinga 2. It offers only `linux/amd64`.
    - How a Windows artifact is built and verified is decided with that run.

23. **The kind refuses to start on Windows** (clause 22), naming why. So Windows hosts are not
    served by it. A block still passes `Plugin::check` on Windows, so one Supervisor set stays
    valid across a fleet.

24. **The artifact document is `docs/artifacts/icinga2.md`.** Two tests hold it:
    - one on the repack plan in `opamp-fleetctl package fetch`: the Debian repack, the wrapper
      directory, the output name;
    - one on the kind's constants, `cfg`-gated per platform.

25. **A delivered block reads files only from its configuration directory, and pins the parent it
    names.**
    - The kind states `ticket_file` and `trusted_cert_file` as the settings that name a file the
      Supervisor reads ([CLT-supervisor-mode-and-kinds §36](CLT-supervisor-mode-and-kinds.md)).
    - In a delivered block each must be `${config_dir}/` followed by a relative path with no `..`,
      `.` or root component.
    - The one exception is that of
      [CLT-supervisor-mode-and-kinds §36](CLT-supervisor-mode-and-kinds.md). The setting equals
      the running block's value, and the block still names the same parent, `node_name` and
      `trusted_cert_file`.
    - Those three are the settings that say where the file's content goes. A pin the Server swapped
      would hand the operator's ticket to whoever presents that certificate under the parent's name.
    - `node_name` names the host's certificate and key files. It is a plain name with no separator
      in every block.
    - A delivered `icinga2` block that names a `parent_host` must name `trusted_cert_file`. Trust on
      first use is for a parent an operator chose, not one the Server names.
    - Clauses 2 and 12 stand otherwise for an operator-written block.

26. **A listed `icinga2` keeps the parent and the pin the operator wrote.**
    - For a Supervisor named in `remote_config_disabled`, a delivered block equals the running one
      whole ([CLT-supervisor-mode-and-kinds §51](CLT-supervisor-mode-and-kinds.md)).
    - `node_name`, `parent_host`, `ticket_file` and `trusted_cert_file` are among it.
    - An added listed `icinga2` carries `type` and `name` alone. It is thus a standalone Icinga
      instance until the operator writes its parent into the host's `supervisor.toml`.
    - From then on the running block is the one a delivered block must equal.

27. **A later repack of one Icinga version is numbered as the operator program numbers one.**
    - The first repack of Icinga `<major>.<minor>.<patch>` is Package `<major>.<minor>.<patch>`
      (clause 17).
    - A later one of the same version brings newer library packages, check plugins or packaging
      revision. `opamp-fleetctl package fetch --repack <n>` names it.
    - [OPS-operator-program-fleetctl §28](OPS-operator-program-fleetctl.md) numbers it. The second
      repack of 2.14.3 is `2.14.4-0.r.2`.
    - It is offered to a host on any earlier repack. That host's Supervisor reports the version of
      the Package it installed, not the `2.14.3` the daemon prints
      ([SRV-deployments-and-pinned-rollouts §14 and §25](SRV-deployments-and-pinned-rollouts.md)).
    - Icinga's next release `2.14.4` replaces it.

**Out of scope:**

- the RPM repack. Only a deployment that must run the vendor's own Red Hat binary needs it, and it
  brings subscription credentials;
- running on Windows (clauses 22 and 23);
- an explicit renewal schedule beyond the start-time net;
- holding the enrolment values as fleet concepts rather than block keys;
- running on a host where a process account is registered
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)). There a block is refused
  when it is checked and when it starts, naming `--no-process-account`. The kind runs `pki` and
  `daemon -C` itself, outside the second service
  ([CLT-accounts-service-runs-under §18](CLT-accounts-service-runs-under.md)), and names the
  Client's account as `-D RunAsUser` (clause 5). So both would run as that account, past the
  second service.

## Alternatives considered

- **A recipe under the generic `command` kind.** It can express the arguments but not the directory
  preparation, the certificate gate, or the validation; its failure mode is a fleet reporting
  `APPLIED` for a configuration Icinga refused, and a crash loop while a host waits for a
  certificate.
- **Operator-written hook keys** (`pre_start_cmd`, `validate_cmd`). Rejected: a key that is the
  mechanism moves the decision into every host's file and makes the Supervisor unreviewable.
- **Paths as overrides with derived defaults**, or **probing the tree** for `sbin/icinga2*`.
  Rejected: the values are the artifact's, so an override can only be wrong or redundant, and a
  wrong guess is silent where a constant is testable.
- **A required root name `icinga2-conf`, no role.** Rejected: a fleet naming its Configurations
  itself would have nothing to say which is the root.
- **Drop `node_name` and always use the FQDN.** Rejected: a host enrolled under another CN could not
  be expressed, and the failure is a refused enrolment.
- **Require a `nagios` user on the host.** A provisioning step outside the fleet, for nothing.
- **Let the fleet Server issue Icinga certificates.** Rejected: a second trust root in someone
  else's monitoring topology that no master would trust, doubling a Server compromise's reach.
- **Deliver certificate and private key as Configurations.** Rejected outright: a central key in the
  Server's store, readable to whoever reads the fleet.
- **Trust on first use by default**, or **a ticket in every host's file.** Rejected: the first hands
  an attacker in the path a permanent foothold; the second puts a per-host secret in a file the
  fleet rewrites.
- **Build Icinga 2 from source** with our own prefix. Rejected while repacking works: a compile step
  per architecture and a moving dependency list.
- **Install the vendor `.deb`/`.rpm`/MSI on the host.** An installation beside the fleet, needing
  root, a package manager and a service the Client does not supervise.
- **Bundle glibc and its loader.** Rejected: the program would have to be the loader, which a
  program path cannot express, and a mismatched loader/libc pair fails worse than a clear refusal.
- **`patchelf` the RUNPATH.** Unnecessary, and it rewrites a binary whose checksum was just
  verified.
- **One artifact per distribution family.** Stricter than the constraint it derives from, doubles
  build and test surface, and for Red Hat needs a subscription — while a Debian-built tree already
  serves those hosts.
- **`<major>.<minor>.<patch>+r<n>` for a later repack.** Rejected: build metadata decides no
  order, so the Server would never offer it to a host on the first repack. **`<version>-r<n>`**
  orders below the version it repacks. **Finding the next number on the Server** would make the
  tool read the Package store before it builds, for a number the operator states in one flag.
- **Repack the newest suite.** Quietly excludes the hosts most likely to run an old agent.
- **Copy the libraries the build host resolves (`ldd`).** Rejected: the tree's content would be
  whatever that host has installed, from wherever it came, which
  [OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md) rules out for a repack;
  `ldd` runs the host's loader on the program; and the build host's distribution would become the
  reach, so a repack for another suite would need a container of that suite with the right libraries
  installed.
- **Trust the build host's `debian-archive-keyring`.** Rejected for the same reason: the host
  would decide which keys vouch for the tree. The carried keyring rotates with a tool release,
  which a reviewer checks by fingerprint.
- **Ignore `Valid-Until`.** Rejected: a signature does not expire, so an index from before a
  security update would verify for ever.
- **A crate for `.xz`.** Rejected while `xz` is on every Debian host: its output is checked against
  the signed digest anyway, and a decompressor adds a dependency to an operator tool for one file.
- **The tool installing libraries.** It makes an operator tool run `apt-get install`.

## Sources / Prior art

- Spikes against Icinga 2.14.6-1 (Debian trixie) and 2.16.4/2.16.5: the `-D` matrix, the
  `RunAsUser` refusal, `-I` vs. `IncludeConfDir`, pid stability across `SIGHUP`, the silent reload
  abort, the orphaned worker on `SIGKILL`, `readelf -d` (RUNPATH), the library closure, `objdump -T`
  for the `GLIBC_` floor, a 39 MB / 52-file relocated tree, and the `pki` subcommands with explicit
  paths.
- [Icinga 2 — CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/),
  [Distributed Monitoring](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/)
  (tickets, CSR signing, the signing queue, `NodeName` = CN = Endpoint, FQDN default),
  [Configuration](https://icinga.com/docs/icinga-2/latest/doc/04-configuration/) (`-D` constants,
  `object FileLogger`),
  [language reference](https://icinga.com/docs/icinga-2/latest/doc/17-language-reference/).
- [`icinga-app/icinga.cpp`](https://github.com/Icinga/icinga2/blob/master/icinga-app/icinga.cpp) —
  where the path constants come from, and that `-D` is applied before they are frozen.
- [packages.icinga.com](https://packages.icinga.com/) — the vendor repositories, the `Depends:
  libc6` per build, the open EL repository ending at 8 and `subscription/` answering `401`, and the
  Windows MSIs without digests (`Icinga2-v2.16.4-x86_64.msi`).
- [`packages.icinga.com/icinga.key`](https://packages.icinga.com/icinga.key) — the archive key;
  `gpgv` against `dists/icinga-bookworm/InRelease` (`VALIDSIG`), and Debian's `InRelease` signed by
  several keys at once.
- Icinga's
  [`lib/remote/pkiutility.cpp`](https://github.com/Icinga/icinga2/blob/v2.14.6/lib/remote/pkiutility.cpp)
  and
  [`jsonrpcconnection-pki.cpp`](https://github.com/Icinga/icinga2/blob/v2.14.6/lib/remote/jsonrpcconnection-pki.cpp)
  — `status_code` 2 for a queued request, exit status 0, the request filed by certificate
  fingerprint.
- glibc's symbol versioning — why "built old, runs new" holds and its converse does not; Debian's
  glibc per release (bookworm 2.36); `objdump -T` over the repacked 2.16.5 tree (`libstdc++.so.6`
  `GLIBC_2.36`, everything else at most `GLIBC_2.35`).
- [Debian archive signing keys](https://ftp-master.debian.org/keys.html) — the automatic signing
  keys of Debian 11, 12 and 13 the carried keyring holds; `gpgv` against
  `debian-security/dists/bookworm-security/InRelease` (`VALIDSIG` by subkeys, primary last).
- [`archive.debian.org`](https://archive.debian.org/), read on 2026-10-28: `bullseye`,
  `bullseye-updates` and `bullseye-security` there verify against the carried keyring by the
  11/bullseye and 12/bookworm keys and state no `Valid-Until`; `bullseye-security`'s `InRelease` is
  byte for byte the one `deb.debian.org` serves, whose pool answers `404` for five of the library
  packages a bullseye repack of Icinga 2 2.16.5 needs, all five served at the same path there.
- [`apt-secure(8)`](https://manpages.debian.org/bookworm/apt/apt-secure.8.en.html) and
  [the repository format](https://wiki.debian.org/DebianRepository/Format) — `InRelease`,
  `Valid-Until`, `by-hash`, the `Packages` index and its digests.
- [ELF `DT_NEEDED`](https://refspecs.linuxfoundation.org/elf/gabi4+/ch5.dynamic.html) — the dynamic
  section the libraries are read from.

## Consequences

- Positive: Icinga 2 is an ordinary fleet citizen — rolled out, updated, rolled back and configured
  like any Managed Process, with no host-side installation, carrying its own libraries, ITL and
  check plugins.
- Positive: the derived arguments are written once, in code; one Supervisor set can configure every
  Icinga host, with `parent_host` fleet-wide.
- Negative: an Agent enrolled with a ticket is deployed alone until it has its certificate: the
  ticket reaches it only through a Deployment of its own, which must hold everything it runs
  meanwhile (clause 11).
- Positive: the private key is generated where it is used; a compromised fleet Server leaks at most
  a ticket bound to one CN.
- Positive: the reach is the suite named rather than the host a build ran on, any host builds the
  same tree from the same indexes, and every file of it is bound to Icinga's or Debian's signing
  keys rather than to a mirror's honesty or the build host's state.
- Negative: the libraries are Debian's at the versions the indexes state on the day of the
  repack; a security update to one of them, OpenSSL above all, reaches the fleet only with a new
  repack of the same Icinga version.
- Negative: the bookworm artifact does not reach Ubuntu 22.04 or RHEL 9, because the `libstdc++6`
  it carries needs glibc 2.36; those hosts need the bullseye repack, which reads what
  `deb.debian.org` no longer serves of Debian 11 from `archive.debian.org` (clause 18) and so
  depends on Debian keeping that archive.
- Negative: a third plugin kind that shells out to its own program for validation and enrolment and
  depends on its exit codes and output, bounded by a timeout.
- Negative: the daemon runs under the Client's account, so checks needing elevated capabilities
  (`check_icmp`) fail unless that account is root, which `service install --run-as root` chooses
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)). Documented, not
  worked around.
- Negative: the kind refuses a process account, so a delivered Icinga configuration runs commands
  as the Client's account, root on many hosts. Icinga's configuration language runs programs: a
  `CheckCommand` names one, and `accept_commands` lets the parent run the commands it defines.
  Whoever changes what the Server delivers to the Supervisor can therefore run code on the host
  as that account. Where Q-5 must hold against the Server, the operator names the Supervisor in
  `remote_config_disabled` ([`HARDENING.md`](../HARDENING.md),
  [`docs/manual/client.md`](../manual/client.md)); clause 26 then keeps its block the operator's.
  `docs/manual/icinga2.md` says the same.
- Negative: a host awaiting on-demand signing looks unhealthy until someone signs; the manual's
  troubleshooting table says so.
- Negative: a differently packed tree does not fit, and an operator who renames the root
  Configuration and forgets the role gets a Supervisor that will not start; the message names both
  ways out.
- Negative: the artifact carries the build distribution's OpenSSL layout to other families, and
  running a vendor binary on a family it was not built for is a support question this project
  cannot answer.
- Negative: a new repository key, or a new Debian release's signing key, breaks the build until
  the carried anchor changes; the anchors are files a reviewer checks by fingerprint.
- Negative: four more fuzz targets run in CI, one per parser of what the repack downloads
  (clause 20).
- Negative: a repack is numbered by the operator, not found on the Server; a `--repack` that
  repeats one already uploaded names a Package that exists.
- Follow-ups: building a Windows artifact and running it on a Windows host; the RPM path; an
  explicit renewal schedule; the enrolment values as fleet concepts.

## Enforcement

- `crates/fleet-agent/src/supervisor/icinga2.rs`:
  - the block and the command line (clauses 2 to 5):
    `the_layout_follows_the_delivered_tree_and_needs_no_settings`,
    `the_daemon_arguments_carry_the_relocation`, `the_defaults_are_the_artifacts`,
    `a_parent_carries_its_port_or_icingas_default`,
    `a_retired_key_is_refused_by_name_and_says_what_supplies_it_now`,
    `check_refuses_a_block_that_names_a_retired_key`, `settings_parse_strictly`,
    `only_a_qualified_name_becomes_the_default`, `a_configured_node_name_outranks_the_resolved_one`,
    `a_validation_names_the_account_once`, `every_subcommand_carries_the_trees_environment`;
  - the process account (Out of scope): `a_host_with_a_process_account_refuses_an_icinga2_block`;
  - the root and the gate (clauses 6 to 8):
    `the_marked_entry_is_the_root_even_beside_the_conventional_name`,
    `two_marked_entries_are_a_reason_not_to_start`,
    `the_daemon_waits_for_its_configuration_and_its_certificate`,
    `a_standalone_node_needs_no_certificate`, `preparing_creates_every_state_directory`;
  - the validation (clause 9): `a_configuration_icinga_refuses_does_not_reach_the_daemon`,
    `a_refused_configuration_is_not_started_at_the_next_spawn_either`,
    `a_validation_that_did_not_run_records_no_refusal`;
  - the enrolment (clauses 11 and 12): `enrolment_obtains_a_certificate_once`,
    `an_unreachable_parent_is_reported_rather_than_recorded`,
    `a_queued_request_is_asked_again_with_the_same_key_until_the_parent_signs`,
    `the_ticket_and_the_parent_certificate_reach_the_supervisor_and_not_the_daemon`,
    `trust_on_first_use_pins_a_parent_once`,
    `a_named_parent_certificate_that_is_missing_is_waited_for_not_trusted_on_sight`,
    `a_named_ticket_that_is_missing_is_waited_for`, `a_failed_request_never_shows_the_ticket`;
  - the renewal (clause 13): `an_expired_certificate_enrols_again`,
    `the_expiry_is_read_from_what_pki_verify_printed`,
    `a_certificate_near_expiry_is_renewed_without_a_new_key`,
    `a_renewal_whose_named_parent_certificate_is_gone_waits_for_it`,
    `a_verification_that_did_not_run_keeps_the_certificate`,
    `a_subcommand_that_never_answered_is_told_from_a_refusal` (clauses 9 and 13);
  - the wait (clause 14): `a_failed_renewal_keeps_the_daemon_on_the_certificate_in_force`;
  - the preflight and the version (clauses 15 and 16):
    `the_preflight_runs_the_staged_tree_against_its_own_libraries`,
    `the_version_banner_is_read_without_its_packaging_revision`;
  - Windows (clause 23): `the_kind_refuses_to_start_on_windows_until_it_is_proven_there`.
- `crates/fleet-agent/tests/icinga2_supervisor.rs`, against the `stub_icinga2` binary:
  - `the_daemon_is_handed_no_endpoint_token` (clause 1);
  - on Linux, `a_stop_ends_the_worker_with_the_daemon` (clause 10). The stub's umbrella and its
    worker both ignore `SIGTERM`, and the escalation to `SIGKILL` ends both;
  - `an_unreachable_parent_waits_with_a_reason_and_starts_nothing`,
    `enrolment_opens_the_gate_and_the_daemon_starts`,
    `a_configuration_icinga_refuses_is_reported_failed`,
    `a_refused_configuration_is_not_started_again_and_the_health_says_why` (clauses 8 and 9);
  - `a_standalone_node_runs_without_enrolment`;
  - `the_version_is_probed_with_the_trees_environment` (clauses 5 and 16).

  The stub runs no subcommand without one tree's `lib/` in `LD_LIBRARY_PATH`. So every test that
  drives it holds clause 5.
- `crates/fleet-agent/src/reconfigure.rs`:
  - clause 25: `a_delivered_icinga2_block_cannot_send_the_operators_ticket_elsewhere`,
    `a_delivered_icinga2_block_reads_files_only_from_its_config_dir`,
    `a_delivered_icinga2_block_must_pin_its_parent`;
  - clause 26: `a_listed_icinga2_keeps_the_parent_and_the_pin_the_operator_wrote`. A delivered
    block for a listed name fails the offer, naming the key, where it changes `parent_host` or
    `node_name`. So does one that points `trusted_cert_file` at a missing file in `${config_dir}`.
- `crates/fleet-agent/src/supervisor/mod.rs`, with this kind standing for every wrapped kind
  ([CLT-supervisor-mode-and-kinds §13 and §17](CLT-supervisor-mode-and-kinds.md)):
  `a_wrapped_block_needs_neither_a_program_nor_a_type`,
  `a_wrapped_block_that_restates_a_derived_value_is_refused`,
  `a_wrapped_kind_corrects_the_fleets_timing_and_the_block_says_nothing` (clause 9's timing).
- `crates/fleet-tools/src/fetch.rs`:
  - the reach (clauses 19 and 22): `icinga_2s_line_is_the_reach_of_the_host_it_is_read_on`,
    `every_distro_the_tool_builds_for_has_a_stated_reach`,
    `any_host_repacks_any_suite_and_defaults_to_its_own`;
  - the signatures and the index (clause 18): `the_carried_key_verifies_icingas_own_signed_index`,
    `the_carried_debian_keys_are_the_archives_own`,
    `the_carried_keys_verify_debians_own_signed_index`,
    `a_signed_index_served_for_another_suite_is_refused`,
    `an_index_past_its_valid_until_is_refused`,
    `an_index_is_read_only_as_its_signed_release_states_it`,
    `the_repository_index_yields_a_packages_filename_digest_and_libc_floor`,
    `the_newest_revision_is_taken_in_debians_order`;
  - Debian's archive (clause 18): `only_debians_own_repositories_move_to_its_archive`,
    `a_file_debian_no_longer_serves_is_read_from_its_archive_and_only_then`,
    `an_archived_suite_verifies_by_the_same_keys_and_states_no_valid_until`;
  - the packages and the libraries (clause 18):
    `the_library_packages_are_the_depends_closure_in_the_signed_index`,
    `the_libraries_a_program_needs_are_read_without_running_it`,
    `the_tree_takes_its_libraries_from_the_archives_packages_and_nothing_from_the_host`,
    `a_copy_out_of_an_unpacked_package_follows_no_link_out_of_it`,
    `a_link_out_to_a_directory_stops_the_next_unpack`,
    `an_alternative_names_only_a_place_inside_the_payload`,
    `an_alternative_is_written_only_where_nothing_is_and_through_no_link`;
  - the numbering (clause 27): `a_later_icinga_repack_is_numbered_as_the_core_numbers_one`;
  - the packing side of clause 24: `icinga_2s_linux_artifact_is_the_debian_tree_under_its_wrapper`.
- The caps of clause 20, in `crates/fleet-tools/src/fetch.rs`:
  `an_inrelease_over_its_cap_is_refused_up_front`,
  `an_index_longer_than_its_signed_size_is_refused_up_front`,
  `a_package_is_fetched_within_its_indexed_size_and_never_without_one`, and
  `the_repository_index_yields_a_packages_filename_digest_and_libc_floor` for the size the index
  states.
- The fuzz targets of clause 20: `fuzz/fuzz_targets/deb_release.rs`, `deb_index.rs`,
  `deb_alternatives.rs` and `elf_needed.rs`. `scripts/check-fuzz-targets.sh` holds them listed in
  `fuzz/Cargo.toml`, the fuzz workflow's matrix and `fuzz/seeds/`. The fuzz crate's build fails on
  a `crate::` path in `crates/fleet-tools/src/fetch/debian.rs`.

**Not mechanically decidable:**

- whether a repacked tree runs on Windows (clause 22), which needs a Windows host;
- that the ticket reaches one Agent through a Deployment of its own (clause 11). That is the
  Selector behaviour of
  [SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md) and the
  procedure `docs/manual/icinga2.md` gives, which no test of this kind holds.

Review holds these.
