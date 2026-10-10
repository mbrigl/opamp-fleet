# ADR-0019: Icinga 2 runs as the `icinga2` kind from a repacked vendor tree, enrols with its Icinga master, and reaches the hosts whose glibc is at least its build host's

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/icinga2.rs, the preflight, version parser and process-group stop in crates/fleet-agent/src/supervisor/process.rs, icinga2_plans and windows_plan in opamp-package-fetch, the Dev Container image and its system packages, docs/artifacts/icinga2.md

## Context

Icinga 2 should reach a host the way every other Managed Process does: a package the Server
offers and the Client unpacks, updates and rolls back, never a distribution package installed
beside the fleet. The target is the Icinga **Agent** role, which needs a certificate signed by an
Icinga master. A kind knows its own agent ([ADR-0017](0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)), and
Icinga 2 is the agent where a kind is not merely shorter than a recipe but required. A spike against
Icinga 2.14.6 measured why. A repacked tree runs from any directory — `strace` showed no compiled-in
path touched — but only under conditions no block can carry:

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
so a tree's reach is one number: the build host's glibc. The vendor states it per build:

| Vendor build | `Depends: libc6 (>= …)` | Runs on |
|---|---|---|
| Debian 11 (bullseye) | 2.30 | Debian 11+, Ubuntu 20.04+, RHEL 9+ |
| Debian 12 (bookworm) | 2.34 | Debian 12+, Ubuntu 22.04+, RHEL 9+ — not RHEL 8 (2.28) |
| Debian 13 (trixie) | 2.38 | Debian 13+, Ubuntu 24.04+, RHEL 10+ |

Icinga's open RPM repository ends at EL 8; from RHEL 9 on it is behind a subscription. The Windows
MSIs at `packages.icinga.com/windows/` publish no digest sidecar at all, but carry an Authenticode
signature by `O=Icinga GmbH`, issued by GlobalSign GCC R45 CodeSigning CA 2020; only the chain to a
root fails on a Linux CA bundle, which holds no code-signing roots.

## Decision

We will run Icinga 2 in the Agent role as a compiled-in `icinga2` kind that derives its whole
command line, gates starts and applies on what Icinga needs, and enrols with the Icinga master
over the `pki` subcommands — from a link-free tree `opamp-package-fetch` repacks out of the vendor's
packages, bundling everything but glibc and built on a pinned Dev Container whose glibc is the
artifact's reach.

1. **A kind of its own, reusing the shared Runner.** One module in
   `crates/fleet-agent/src/supervisor/` and one registry line; spawn, watchdog, backoff, bounded stop,
   package swap, rollback, retention and health are the shared `Runner`'s. The kind supervises the
   **Agent role, never a master**, and runs the delivered tree only: program `icinga2` plus
   `EXE_SUFFIX` at `program_path` `sbin/icinga2[.exe]`, `service_name = "icinga2"`. It refuses an
   `endpoint_port`: Icinga speaks its cluster protocol to its parent, not OpAMP to us.

2. **The block holds the enrolment, and nothing else.** Four optional keys, parsed strictly:

   | Key | What the Agent does with it |
   |---|---|
   | `node_name` | its `NodeName`, the CN of its certificate, its Endpoint name |
   | `parent_host` | the parent it enrols with and connects to, `host` or `host:port` (port defaults to 5665; a bracketed IPv6 address may carry a port, a bare one never does) |
   | `ticket_file` | the file holding the ticket it presents when asking for a certificate |
   | `trusted_cert_file` | the parent's own certificate, pinned instead of trusted on sight |

   No `parent_host` means a standalone node: no enrolment, local checks only. All four are
   rollable: the `[[supervisor]]` blocks are the fleet-managed half of the Client's configuration
   ([ADR-0017](0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)), and the two files travel as
   Configurations (clause 11).

3. **`node_name` defaults to the host's FQDN.** Resolved with `getaddrinfo` and `AI_CANONNAME` (the
   route `hostname --fqdn` takes), accepted only if it contains a dot, once per process and only
   when no `node_name` is set; failing that, the Supervisor's name. Not `host.name`, which is
   `gethostname` and usually the short name — a default that fails enrolment. On Windows nothing is
   resolved. The key stays because a host whose master knows it under another CN must be able to
   say so: a mismatch fails enrolment.

4. **Every other key is the kind's, and refused by name.** A block carrying `binary`,
   `program_path`, `service_name`, `include_dir`, `plugin_dir`, `data_dir`, `log_dir`, `cache_dir`,
   `spool_dir`, `run_dir`, `log_level`, `renew_before_days`, `parent_port`, `run_as_user`,
   `run_as_group`, `args`, `main_config` or `env` fails at startup — and an offered Supervisor set
   fails `Plugin::check` — naming what supplies the value. Logging verbosity belongs in Icinga's own
   `object FileLogger` `severity`, which the fleet rolls out.

5. **The kind derives the command line.** `daemon -c <root> -D RunAsUser=… -D RunAsGroup=…
   -D NodeName=… -D IncludeConfDir=<tree>/share/icinga2/include -D PluginDir=<tree>/plugins
   -D DataDir=… -D LogDir=… -D CacheDir=… -D SpoolDir=… -D InitRunDir=… -x information`, where
   `<tree>` is `${supervisor_dir}/program/tree`:
   - The account is the one the Client runs as (`id -un`/`-gn` on Unix); every `icinga2`
     subcommand carries it. Windows Icinga drops no privileges, so nothing is named there.
   - On Windows `PluginDir` is `<tree>/sbin`: the check plugins sit beside the daemon and share its
     DLLs.
   - The state directories are `${supervisor_dir}/data`, `log`, `cache`, `spool` and `run`.
   - On Unix the daemon runs with `LD_LIBRARY_PATH=<tree>/lib`; Windows needs no environment.
   - Foreground: no `-d`, no `--close-stdio`; stdout and stderr go into the Client's logging.

6. **The kind prepares Icinga's directories.** Before every spawn and before enrolment it creates
   `data/`, `data/certs/`, `log/`, `cache/`, `spool/` and `run/` owner-only, so one removed under a
   running fleet comes back. It runs no `prepare-dirs`, creates no users, and drives no service
   manager. State lives **beside** the tree — `data/` (with the certificates), the enrolment marker
   `icinga2-enrolment.json` and the pinned `trusted-parent.crt` are siblings of `program/` and
   `config/` — so a package swap never touches the identity, and purging the Supervisor's directory
   takes all of it.

7. **The root Configuration is marked by `role = "main"`.** The daemon is pointed at one file, from
   which it `include`s the rest, and that root cannot be derived from unroled entries. The
   Baseline makes `AgentConfigFile.role` Agent-type-specific (*"The values and their semantics are
   Agent type-specific"*), so `main` is the `icinga2` kind's own value, read by no other kind (to a
   Collector any non-empty role means "written, never passed as `--config`",
   [ADR-0025](0025-configurations-and-the-rest-api.md)). Where no entry carries it, the entry named
   `icinga2-conf` — the one `opamp-package-fetch` uploads — is the root. Two entries carrying it are
   a reason not to start, naming both. The root is resolved on every spawn.

8. **No process until it can do its job.** The kind's `build()` yields no process while the root
   Configuration is missing or — with a parent configured — while `<NodeName>.crt` and `ca.crt` are
   missing from `data/certs/`. The Runner then reports what it is waiting for instead of
   crash-looping toward a hold.

9. **A Configuration is validated before it is applied.** `daemon -C` runs against the delivered
   configuration with the same command line. On failure the running daemon is not touched — not
   stopped, not reloaded — and the apply is answered `ConfigApplied{Err}` with the validator's
   message. On success the apply goes to the Runner: `SIGHUP` on Unix (the umbrella's pid is
   stable), restart on Windows. Icinga's slow drain makes `stop_timeout` 60 s and `apply_grace` 30 s
   the kind's timing.

10. **The daemon is stopped as a group.** It is started in its own process group, and the Runner's
    stop signals the group, so the escalation to `SIGKILL` cannot leave the worker behind. Leading a
    group is opt-in per kind (`ProcessSpec::own_process_group`); kinds with one process keep
    signalling the pid. Windows has no equivalent yet.

11. **The Icinga master is the only CA; the fleet transports, never signs.** The fleet Server holds
    no `TicketSalt`, signs nothing, and never sees a private key; the OpAMP PKI of
    [ADR-0022](0022-admission-by-a-client-certificate-alone.md) and Icinga's never touch. It carries two
    artefacts that are not keys: the **ticket** (useless for any other CN) as a Configuration with
    `role = "supplementary"` and a Selector matching one Agent, and the **parent's certificate**
    (public), the same way. Both land as files the Supervisor reads and the daemon is never pointed
    at.

12. **Enrolment is `pki`, never `node setup`, and runs in the adapter.** A task beside the Runner —
    never `Plugin::start`, so an unreachable master cannot hold up the Client's startup — runs, each
    subcommand bounded at 30 s:
    - `pki new-cert` with the CN and explicit key/certificate paths under `data/certs/`; the key
      never leaves the host;
    - the parent pinned: the delivered `trusted_cert_file` is **copied** to `trusted-parent.crt`,
      because the next apply empties the entry directory; only when none was delivered,
      `pki save-cert` — trust on first use — logged as such;
    - `pki request` against the parent with the pinned certificate and, when a ticket was
      delivered, `--ticket`. Without a ticket the request waits in the master's queue for
      `icinga2 ca sign`, and the Agent stays unhealthy saying so.

    On success the marker is written and the Runner is sent a restart, which reopens the gate of
    clause 8.

13. **The certificate on disk is the state; the marker is a hint.** With a certificate, a CA and a
    marker naming the same CN, parent host and port, `pki verify` decides: valid beyond 30 days,
    nothing runs; within 30 days of expiry, or with an unreadable expiry, a **renewal** — the key is
    kept, no `new-cert`, no ticket; the certificate in force authenticates its own renewal. A
    certificate that does not verify, or a changed CN or parent, enrols again. Renewal is otherwise
    the daemon's own; this is a start-time safety net.

14. **An unreachable master is a wait.** A failed attempt reports health `awaiting the certificate
    for <node>` with the reason, backs off, and retries; no daemon is started. Revocation stays on
    the master: removing the Supervisor removes the key material with its directory, and
    `icinga2 ca remove` there is the operator's.

15. **A package is proved to run before it is installed.** For every kind, after an artifact is
    unpacked into `program/.staging` and before the swap, the Runner runs the staged program once
    with a plugin-supplied preflight. A failure answers `PackageApplied{Err}` carrying the dynamic
    linker's own message (*"version `GLIBC_2.39' not found"*, *"cannot open shared object file"*),
    and nothing is swapped, so a running Managed Process is never stopped for a package that could
    not run. Icinga's preflight is `--version` with `LD_LIBRARY_PATH=${staged}/lib`; the other kinds
    use their version arguments (`command`: its `version_args`). The health gate and rollback of
    [ADR-0028](0028-packages-signed-deployments-offered-downloads-and-verified-delivery.md) remain the second line.

16. **A version probe may bring its own parser.** `VersionProbe` takes an optional parse function,
    strict SemVer by default; Icinga's reads `r2.14.6-1` as `2.14.6`.

17. **The artifact is the vendor's packages, repacked into one normalised, link-free tree.**
    `opamp-package-fetch --agent icinga2` builds, under the wrapper `icinga2-<version>`:

    ```
    sbin/icinga2[.exe]      the daemon (Windows: and the check plugins)
    lib/                    the gathered libraries (Linux)
    share/icinga2/include/  the ITL
    plugins/                the check plugins (Linux)
    doc/                    the vendors' copyright files
    ```

    Left out: `/etc/icinga2` (the fleet delivers configuration), the systemd unit and init script,
    `prepare-dirs` and `safe-reload`, documentation beyond copyright. Repacking is redistribution,
    so the copyright files travel. The Linux daemon is taken from `usr/lib`, not the `/usr/sbin`
    shell wrapper. The container is **`.tar.gz` on every platform**, Windows included — the only one
    carrying the executable bit. Packing is the deterministic tree packing of
    [ADR-0018](0018-glpi-agent-and-telegraf.md) clause 7. The artifact is uploaded as the Package of
    Agent type `icinga2` at the Icinga version, one entry per platform.

18. **Linux: every library but glibc rides along, verified from the repository index.** The sources
    are `icinga2-bin` and `icinga2-common` from the `icinga-<distro>` suite of
    `packages.icinga.com/debian`, and `monitoring-plugins-basic` and `-common` from
    `deb.debian.org` in the distribution's own version. Each is verified against the `SHA256` its
    `Packages` index states before anything is unpacked. Each program's `ldd` closure minus glibc
    and the loader is copied into `lib/`, links dereferenced; no `patchelf`. A library the build
    host cannot resolve is refused, naming the packages that provide it — a tree missing one would
    ship and die on its first start.

19. **The build host is the reach, and there is one artifact per platform.** The tool builds only
    for the distribution its host is (`VERSION_CODENAME` in `/etc/os-release`; `--distro` naming
    another is refused), and prints the vendor's `libc6` floor from the same index stanza, with the
    reach it implies, before anything is uploaded. Build on the oldest system the fleet serves: the
    artifact serves every newer one, across distribution families. A Debian-built tree carries
    Debian's OpenSSL directory layout — inert for cluster TLS, whose certificates are named by
    explicit paths; the manual names it as the thing to verify for checks using the system trust
    store on Red Hat hosts. The RPM repack is not built.

20. **The Dev Container is the build host, pinned to the reach.** Its image is
    `mcr.microsoft.com/devcontainers/base:debian12` — a release, never the floating `:debian` tag —
    so artifacts built in it declare `libc6 >= 2.34` and reach Debian 12+, Ubuntu 22.04+ and RHEL 9+.
    **The image line is the reach**: changing it decides which hosts the fleet can serve, and the
    comment on it says so. The container installs what `icinga2-bin` needs beyond the base image —
    six Boost 1.74.0 packages and `libprotobuf-lite32`, whose version suffixes move with the pin —
    and the repack helpers `msitools`, `osslsigncode`, `rpm2cpio` and `cpio`. A container of another
    distribution is how a different reach is built.

21. **Extraction helpers are shelled out to and refused by name when missing:** `dpkg-deb`, `ldd`,
    `msiextract`, `osslsigncode`. The tool never installs anything on its host.

22. **Windows: the MSI is verified by its publisher's signature.** Both conditions are required:
    the embedded Authenticode signature verifies against the file's contents, and the signer's
    subject names the expected publisher — `O=Icinga GmbH`, a constant of the agent's definition,
    never a flag. An unsigned, altered or foreign-signed MSI is refused by name and nothing is
    unpacked. The chain to a root is reported as not validated locally, not required. The verified
    payload is unpacked into the same tree; no libraries are gathered, since a Windows program finds
    its DLLs beside itself.

23. **Windows is unproven.** The Windows artifact builds and is verified, but whether the repacked
    tree runs without the MSI's product registration has not been tried on a Windows host, and the
    kind has no other form to fall back on. Until it has, Windows hosts are not served by this kind.

24. **The artifact document is `docs/artifacts/icinga2.md`**, held by two tests: one on the repack
    plan in `opamp-package-fetch` (the Debian repack, the MSI payload, the wrapper directory, the
    output names), one on the kind's constants, `cfg`-gated per platform.

**Out of scope:** the RPM repack (needed only for a deployment that must run the vendor's own Red
Hat binary, and it brings subscription credentials); running on Windows (clause 23) and a job-object
group stop there; validating the Authenticode chain against a carried root; an explicit renewal
schedule beyond the start-time net; holding the enrolment values as fleet concepts rather than block
keys.

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
- **Preflight by static inspection** of required `GLIBC_` symbols. Rejected: a start attempt is the
  definition of "runs here", catches a missing library too, and yields the operator's message.
- **Let the fleet Server issue Icinga certificates.** Rejected: a second trust root in someone
  else's monitoring topology that no master would trust, doubling a Server compromise's reach.
- **Deliver certificate and private key as Configurations.** Rejected outright: a central key in the
  Server's store, readable to whoever reads the fleet.
- **Trust on first use by default**, or **a ticket in every host's file.** Rejected: the first hands
  an attacker in the path a permanent foothold; the second puts a per-host secret in a file the fleet
  rewrites.
- **Build Icinga 2 from source** with our own prefix. Rejected while repacking works: a compile step
  per architecture and a moving dependency list.
- **Install the vendor `.deb`/`.rpm`/MSI on the host.** An installation beside the fleet, needing
  root, a package manager and a service the Client does not supervise.
- **Bundle glibc and its loader.** Rejected: the program would have to be the loader, which a
  program path cannot express, and a mismatched loader/libc pair fails worse than a clear refusal.
- **`patchelf` the RUNPATH.** Unnecessary, and it rewrites a binary whose checksum was just verified.
- **`.7z` or `.zip` for Windows.** They carry no executable bits.
- **One artifact per distribution family.** Stricter than the constraint it derives from, doubles
  build and test surface, and for Red Hat needs a subscription — while a Debian-built tree already
  serves those hosts.
- **Build on the newest distribution.** Quietly excludes the hosts most likely to run an old agent.
- **Keep the floating image tag.** The reach of a shipped artifact would change the day upstream
  retags, with no commit to review. **Pin to bullseye** (2.30) for wider reach: deferred until a host
  needs it, since it dates the whole development environment. **A throwaway build container**, or
  **the tool installing libraries**: the first leaves the Dev Container unable to run a tool this
  repository ships, the second makes an operator tool run `apt-get install`.
- **An operator-supplied `--sha256`**, or **trusting TLS to `packages.icinga.com`**, for the MSI.
  Rejected: both put the artifact's integrity in the hands of whoever serves or copies from that
  page; a signature binds it to a key the mirror does not hold. **Verifying on a Windows host**
  would make the artifact harder to produce than to trust.

## Sources / Prior art

- Spikes against Icinga 2.14.6-1 (Debian trixie) and 2.16.4/2.16.5: the `-D` matrix, the
  `RunAsUser` refusal, `-I` vs. `IncludeConfDir`, pid stability across `SIGHUP`, the silent reload
  abort, the orphaned worker on `SIGKILL`, `readelf -d` (RUNPATH), the `ldd` closure, `objdump -T`
  for the `GLIBC_` floor, a 39 MB / 52-file relocated tree, and the `pki` subcommands with explicit
  paths.
- [Icinga 2 — CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/),
  [Distributed Monitoring](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/)
  (tickets, CSR signing, the signing queue, `NodeName` = CN = Endpoint, FQDN default),
  [Configuration](https://icinga.com/docs/icinga-2/latest/doc/04-configuration/) (`-D` constants,
  `object FileLogger`), [language reference](https://icinga.com/docs/icinga-2/latest/doc/17-language-reference/).
- [`icinga-app/icinga.cpp`](https://github.com/Icinga/icinga2/blob/master/icinga-app/icinga.cpp) —
  where the path constants come from, and that `-D` is applied before they are frozen.
- [packages.icinga.com](https://packages.icinga.com/) — the vendor repositories, the `Depends:
  libc6` per build, the open EL repository ending at 8 and `subscription/` answering `401`, and the
  Windows MSIs without digests (`Icinga2-v2.16.4-x86_64.msi`).
- [`osslsigncode`](https://github.com/mtrojnar/osslsigncode) — the Authenticode verifier.
- glibc's symbol versioning — why "built old, runs new" holds and its converse does not; Debian's
  glibc per release (bookworm 2.36).

## Consequences

- Positive: Icinga 2 is an ordinary fleet citizen — rolled out, updated, rolled back and configured
  like any Managed Process, with no host-side installation, carrying its own libraries, ITL and
  check plugins.
- Positive: the derived arguments are written once, in code; one Supervisor set can configure every
  Icinga host, with `parent_host` fleet-wide and the per-host ticket as its own Configuration.
- Positive: the private key is generated where it is used; a compromised fleet Server leaks at most
  a ticket bound to one CN.
- Positive: the preflight protects every tree package — a Collector built against a newer libc is
  refused before the running one is stopped.
- Positive: the reach is a reviewed line rather than the day a build ran, and the Windows artifact
  is bound to Icinga's signing key rather than to a mirror's honesty.
- Negative: a third plugin kind that shells out to its own program for validation and enrolment and
  depends on its exit codes and output, bounded by a timeout.
- Negative: the daemon runs under the Client's account, so checks needing elevated capabilities
  (`check_icmp`) fail. Documented, not worked around.
- Negative: a host awaiting on-demand signing looks unhealthy until someone signs; the manual's
  troubleshooting table says so.
- Negative: a differently packed tree does not fit, and an operator who renames the root
  Configuration and forgets the role gets a Supervisor that will not start; the message names both
  ways out.
- Negative: the artifact carries the build distribution's OpenSSL layout to other families, and
  running a vendor binary on a family it was not built for is a support question this project
  cannot answer.
- Negative: the Dev Container ages with the oldest host served, grows Boost and ICU, and its image
  pin and library names must move together. The chain behind the MSI signature is not validated
  locally, and a publisher rename breaks the build until the constant changes.
- Follow-ups: running the Windows artifact on a Windows host; the RPM path; chain validation; an
  explicit renewal schedule; the enrolment values as fleet concepts.

## Enforcement

- `crates/fleet-agent/src/supervisor/icinga2.rs`:
  `the_layout_follows_the_delivered_tree_and_needs_no_settings`,
  `the_daemon_arguments_carry_the_relocation`, `the_defaults_are_the_artifacts`,
  `a_parent_carries_its_port_or_icingas_default`,
  `a_retired_key_is_refused_by_name_and_says_what_supplies_it_now`,
  `check_refuses_a_block_that_names_a_retired_key`, `settings_parse_strictly`,
  `only_a_qualified_name_becomes_the_default`, `a_configured_node_name_outranks_the_resolved_one`,
  `the_marked_entry_is_the_root_even_beside_the_conventional_name`,
  `two_marked_entries_are_a_reason_not_to_start`,
  `the_daemon_waits_for_its_configuration_and_its_certificate`,
  `a_standalone_node_needs_no_certificate`, `preparing_creates_every_state_directory`,
  `the_version_banner_is_read_without_its_packaging_revision`,
  `enrolment_obtains_a_certificate_once`, `an_unreachable_parent_is_reported_rather_than_recorded`,
  `an_expired_certificate_enrols_again`, `the_expiry_is_read_from_what_pki_verify_printed`,
  `a_certificate_near_expiry_is_renewed_without_a_new_key`,
  `a_configuration_icinga_refuses_does_not_reach_the_daemon`.
- `crates/fleet-agent/tests/icinga2_supervisor.rs` (against the `stub_icinga2` binary):
  `an_unreachable_parent_waits_with_a_reason_and_starts_nothing`,
  `enrolment_opens_the_gate_and_the_daemon_starts`,
  `a_configuration_icinga_refuses_is_reported_failed`,
  `a_standalone_node_runs_without_enrolment`.
- Preflight: `a_package_that_cannot_run_here_is_refused_without_stopping_what_runs`
  (`crates/fleet-agent/tests/supervisor_process.rs`) and
  `a_package_that_fails_the_configured_version_check_is_refused`
  (`crates/fleet-agent/tests/packages_e2e.rs`).
- `crates/fleet-tools/src/bin/opamp-package-fetch.rs`:
  `the_repository_index_yields_a_packages_filename_digest_and_libc_floor`,
  `icinga_2s_line_is_the_reach_of_the_host_it_is_read_on`,
  `every_distro_the_tool_builds_for_has_a_stated_reach`,
  `the_remedy_names_only_the_packages_that_provide_the_missing_libraries`,
  `a_library_no_dependency_accounts_for_falls_back_to_the_whole_list`,
  `a_windows_artifact_is_accepted_only_when_its_publisher_signed_it`,
  `icinga_2s_windows_artifact_is_the_msi_verified_by_its_publisher`.

**Not mechanically decidable:** the process-group stop (clause 10) has no dedicated test; the Dev
Container pin (clause 20) is a reviewed line in `.devcontainer/devcontainer.json` whose meaning — a
glibc floor — no check reads; and whether a repacked tree runs on Windows (clause 23) needs a
Windows host. Review holds these.
