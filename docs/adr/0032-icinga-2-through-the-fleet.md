# ADR-0032: Icinga 2 through the fleet — a Supervisor kind of its own, the Icinga master as the only CA, and one repacked tree verified at its source

- **Status:** 🟡 proposed
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

### Supervision

Icinga 2 should reach a host the way every other Managed Process does: as a package the Server
offers, that the Client unpacks, updates, and rolls back — never as a distribution package installed
beside the fleet. The Agent role is the target, so each host also needs a certificate signed by an
Icinga master (see the enrolment below).

[ADR-0030](0030-glpi-agent-supervision-and-packages.md) established that a Foreign
Agent needs no plugin of its own when *"every difference between the platforms is a value the block
already expresses — program path, arguments — not a behavior the `Runner` lacks"*. Icinga 2 is the
case where that stops holding, and a spike against Icinga 2.14.6 measured why rather than assuming
it. Running a repacked tree from an arbitrary directory works — no compiled-in path is touched at
all, verified with `strace` — but only under conditions the block cannot carry:

- **Every invocation must name the account it runs under.** Without `-D RunAsUser=`/`-D RunAsGroup=`
  *every* subcommand — `daemon`, `pki`, even a validation — refuses with *"Please re-run this command
  as a privileged user or using the `nagios` account"*, because the compiled-in user does not exist
  on a fleet-managed host and the Client's service account ([ADR-0029](0029-the-linux-service-executes-from-opt.md))
  is not it.
- **The ITL is found through `-D IncludeConfDir=`, not `-I`.** Measured: with `-I` alone, `include
  <itl>` still resolved to the *host's* `/usr/share/icinga2`, silently using a copy the fleet does
  not control — the worst kind of working. `-D IncludeConfDir=` resolves into the delivered tree.
- **Icinga creates none of its directories.** With `DataDir`, `LogDir`, `CacheDir`, `SpoolDir` or
  `InitRunDir` pointing at a path that does not exist, startup fails on the first write. Debian's
  packages solve this with `ExecStartPre=prepare-dirs`, which hard-fails without the `nagios` user
  and is therefore not usable here.
- **A failed reload is silent from the outside.** After `SIGHUP` with a broken configuration the
  daemon logs *"Found error in config: reloading aborted"* **to stderr** and keeps running the old
  configuration, with the same pid and the same worker. A Supervisor that acknowledged the apply
  because the process survived would report `APPLIED` for a configuration that never took effect.
- **A killed umbrella orphans its worker.** Icinga 2 runs as an umbrella process with a worker child.
  `SIGTERM` to the umbrella takes the worker with it, measured, within two seconds — but `SIGKILL`
  leaves the worker running and reparented to init, still holding the data directory, the log file
  and port 5665. The bounded stop of ADR-0011 escalates to exactly that signal when the budget runs
  out, so the escalation can leave a second instance behind on the very host the fleet is managing.

Two further measurements shape the decision rather than force it: `SIGHUP` **keeps the pid** of the
umbrella process (only its worker is replaced), so the `Runner`'s watchdog and the reload of
[ADR-0011](0011-supervisor-mode-core-and-plugins.md) fit Icinga 2 as they are; and `--version`
prints `r2.14.6-1`, which the strict SemVer probe rejects — the same gap ADR-0030 recorded for GLPI.

### Enrolment

An Icinga 2 Agent is useless without a certificate signed by its Icinga master: the `ApiListener`
needs one to connect, and the master will not talk to a node it cannot verify. A fleet-delivered
Icinga 2 therefore has to answer a question GLPI never posed — **who obtains that certificate, and
with what secret.**

The forces are unusually clear:

- **This project already has a CA, and it is the wrong one.** [ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md)
  lets the fleet Server sign client certificates for the OpAMP link. Reusing it for Icinga would make
  the fleet Server an Icinga CA — a second trust root inside somebody else's monitoring topology,
  which nothing in the specification asks for and which no Icinga master would accept anyway.
- **The Icinga flow is ticket-based and non-interactive.** The master computes a ticket as an HMAC of
  the node's common name under its own `TicketSalt`; the node generates a key locally, sends a CSR,
  and receives a signed certificate. The private key never leaves the host.
- **The one-shot bootstrap must not touch `ConfigDir`.** `icinga2 node setup` does the whole dance in
  one command — and writes `zones.conf`, `api.conf` and `constants.conf` into `ConfigDir`, the single
  constant that is not reliably relocatable. The lower-level `pki` subcommands (`new-cert`,
  `save-cert`, `request`, `verify`) take **every path as an argument**; a spike confirmed they touch
  no system directory at all and write the private key `0600` themselves.
- **The Server already has a way to deliver a per-host secret.** [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)'s
  `supplementary` role writes a Configuration entry as a plain file the Supervisor does not pass to
  its process, and [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)'s
  Selectors already aim a Configuration at exactly one Agent. A ticket is precisely that: a per-host
  string that must land as a file and go nowhere else.
- **A Configuration apply empties the entry directory.** Every apply deletes the entry files before
  writing the new set, so anything needed *after* the enrolment must be copied out of it.

### The artifact

The supervision above needs something to supervise. Icinga publishes distribution packages (`.deb`,
`.rpm`) and a Windows MSI — no AppImage, no portable directory, so the shape
[ADR-0030](0030-glpi-agent-supervision-and-packages.md) could take for GLPI (*"the
Windows zip byte-for-byte as published"*) does not transfer.

What the Client's package reader fixes ([ADR-0015](0015-package-delivery-for-managed-processes.md)): a tree may contain
neither symlinks nor hard links — one link refuses the whole archive — at most 10 000 members and
2 GiB unpacked, and only `.tar.gz` carries file modes.

What a spike against Icinga 2.14.6-1 measured, which is what makes this decidable at all:

- The real binary carries **`RUNPATH`, not `RPATH`**. `LD_LIBRARY_PATH` is searched first, so bundled
  libraries win **without `patchelf`** — verified by `ldd` resolving into the relocated tree.
- The closure outside glibc is **20 shared objects** (five Boost libraries, OpenSSL, `libsystemd`,
  `libedit`, `libstdc++`, and their transitive dependencies). No ICU: this build needs no
  `boost_regex`.
- The whole tree — binary, libraries, the 29-file ITL, one plugin — is **39 MB in 52 files**,
  comfortably inside the limits.
- **glibc cannot be bundled**, and the effective floor came out at `GLIBC_2.39` — from a *bundled
  library*, not from the binary, which needs 2.38. A tree built on Debian trixie therefore runs
  neither on Debian 12 nor on RHEL 9.
- Debian splits the payload: `icinga2-bin` holds the binary, **`icinga2-common` holds the ITL**. One
  tree needs both packages.

### Reach

glibc cannot be bundled, so the build host's glibc bounds the artifact's reach. But glibc is
**backward compatible**: a program built against an older one runs against every newer one. So what
a tree can reach is decided by one number, not by which packaging tradition its binary came from —
and the vendor states that number in every package it publishes:

| Vendor build | `Depends: libc6 (>= …)` | Runs on |
|---|---|---|
| Debian 11 (bullseye) | 2.30 | Debian 11/12/13, Ubuntu 20.04+, **RHEL 9** (2.34), RHEL 10 |
| Debian 12 (bookworm) | 2.34 | Debian 12/13, RHEL 9 — not RHEL 8 (2.28) |
| Debian 13 (trixie) | 2.38 | only very recent systems |

Everything else the daemon needs already travels with it — 21 shared objects, measured, with
`LD_LIBRARY_PATH` beating the binary's own `RUNPATH`. So an artifact built on Debian 11 runs on a
RHEL 9 host, and a split by distribution family buys nothing there.

Two further facts, both found by looking rather than by reasoning:

- **Icinga's open RPM repository ends at EL 8.** Everything from RHEL 9 on is behind
  `packages.icinga.com/subscription/`, which answers `401` without credentials. A per-family artifact
  for Red Hat therefore cannot currently be built at all without a subscription — while a
  Debian-built one serves those same hosts today.
- **OpenSSL's configuration directory is compiled in**, and the two families differ: Debian's
  `libcrypto` looks in `/usr/lib/ssl`, Red Hat's in `/etc/pki/tls`. For Icinga's cluster TLS this is
  inert — certificates are named by explicit paths (see the enrolment below) — but anything reaching
  for the *system* trust store on a Red Hat host would find the Debian path.

### The Windows artifact

This project does not bend one rule: **nothing is repacked before it is verified.** For the Linux
artifacts that is answered by the repository index, whose `SHA256` field states a digest per file.

The Windows artifact has no such index. `packages.icinga.com/windows/` is a directory listing of
`.msi` files with **no digest sidecars at all** — `.sha256`, `.sha256sum`, `.asc` and `.md5` all
answer `404`. What the MSI does carry is an **Authenticode signature**, and it checks out where it
matters:

```
Signer: /C=DE/ST=Bayern/L=Nuernberg/O=Icinga GmbH/CN=Icinga GmbH
Issuer: GlobalSign GCC R45 CodeSigning CA 2020
Number of verified signatures: 1
Timestamp Server Signature CRL verification: ok
Error: unable to get local issuer certificate
```

The signature covers the file's contents and is timestamped; only the chain to a root fails, because
a Linux CA bundle carries web PKI roots and not the code-signing roots Windows trusts. That is a
property of the build host, not of the artifact.

Two things make this worth deciding rather than improvising. A signature is **not the weaker
substitute** for a digest here: a digest published beside a file on the same server is only as
trustworthy as that server, while a signature is bound to a key the server does not hold — so an
attacker who controls the mirror can rewrite both file and digest, and cannot forge the signature.
And the decision generalises: any future artifact published without an index — a macOS package, a
vendor's direct download — meets the same question.

## Decision

### A Supervisor kind of its own

We will add a **compiled-in Supervisor Plugin `icinga2`** — one module in
`crates/client/src/supervisor/` and one line in `registry()`, the extension point ADR-0011 named and
ADR-0011 anticipated for *"a kind whose installation is not a file swap"*. It **reuses `Runner`
unchanged** for spawn, watchdog, backoff, bounded stop, package swap, rollback, retention and health.

Bound by this decision:

- **The block states values; the kind derives arguments.** The operator writes the parent, the node
  name, and where things live; the kind assembles `daemon -c … -D IncludeConfDir=… -D RunAsUser=…
  -D RunAsGroup=… -D NodeName=… -D DataDir=… -D LogDir=… -D CacheDir=… -D SpoolDir=…
  -D InitRunDir=… -D PluginDir=… -x …`. Nine derived arguments in `args` on every host is a typo
  waiting to happen, and three of them (`RunAsUser`, `RunAsGroup`, `IncludeConfDir`) are not
  operator choices at all — they follow from the Client's own account and its own directory layout.
- **The program key is `binary`**, as the Collector's is: the thing that gets swapped. Bare name
  means the tree is the Client's and takes packages, an absolute path means the machine's
  (ADR-0019) — **both are supported**, so a platform where the repacked tree turns out not to run
  keeps the same supervision and configuration model with a machine-installed Icinga 2.

- **The kind creates the state directories** before every spawn, under `data_dir` and its siblings,
  0700. It does not run `prepare-dirs`, does not create users, and drives no service manager.
- **Foreground, one child.** `daemon` without `-d` and without `--close-stdio`; stdout and stderr are
  inherited into the Client's logging (ADR-0010).
- **State lives beside the tree, never in it:** `data/` (which holds the certificates), the enrolment
  marker, and the pinned parent certificate are siblings of `program/` and `config/`, so a package
  swap replaces the tree without touching the identity, and ADR-0019's purge still takes everything.
- **A configuration is validated before it is applied.** `daemon -C` runs against the delivered
  configuration first; on failure the running daemon is not touched and the apply is answered
  `ConfigApplied{Err}` with the validator's message. This is the measured case above, and ADR-0011's
  rule that an adapter must not acknowledge what it cannot verify.
- **Reload by `SIGHUP` on unix**, restart on Windows — the existing reload-or-restart of ADR-0011,
  enabled because the umbrella pid was measured to be stable.
- **`build()` yields no process until both the main configuration and a certificate exist.** The
  `Runner` then reports plainly what is missing instead of crash-looping toward a hold.
- **The daemon is stopped as a group, not as a pid.** The process is started in its own process
  group and the stop signals the group, so the escalation to `SIGKILL` cannot leave the worker
  behind. Without this, the one path that is *supposed* to guarantee a stopped process is the one
  that produces a second instance.
- **Three changes to shared code**, stated as such because both existing kinds see them:
  - **A package is proved to run before it is installed.** After the artifact is unpacked into
    `program/.staging` and before the swap, the `Runner` runs the staged program once with a
    plugin-supplied preflight (for Icinga 2: `--version`, ~30 ms, no privileges, no state). A failure
    answers `PackageApplied{Err}` carrying the dynamic linker's own message — *"version `GLIBC_2.39'
    not found"*, *"cannot open shared object file"* — and **nothing is swapped**, so a Managed
    Process is never stopped for a package that could not have run. The health gate and rollback of
    ADR-0015 stay as the second line for what a preflight cannot see.
  - **The version probe may bring its own parser.** `VersionProbe` gains an optional parse function,
    defaulting to the strict SemVer, so `r2.14.6-1` is reported as `2.14.6` instead of not at all.
  - **A process may ask to own its process group**, which is what makes the group stop above
    possible. Opt-in per kind, so the Collector and the `command` kind keep their behaviour; on
    Windows the equivalent (a job object) is left for the platform work, and the kind's stop there
    remains the plain one.

### The Icinga master stays the CA

We will keep **the Icinga master as the only CA**, and have the Supervisor enrol **once, on the
host**, with the ticket and the parent's certificate delivered as ordinary Configurations.

Bound by this decision:

- **The fleet Server signs nothing and holds no `TicketSalt`.** It transports two artefacts that are
  not private keys: the **ticket** (an HMAC over the common name, useless for any other node) and the
  **parent's certificate** (public by nature). No private key ever reaches the Server, and the OpAMP
  PKI of ADR-0013 and the Icinga PKI never touch.
- **The ticket is a Configuration named for the Supervisor's use, with `role = "supplementary"` and a
  Selector matching one Agent.** It lands as a file the Supervisor reads and nothing else consumes.
  A block may also name a ticket inline for a hand-written host; the manual discourages it.
- **The parent's certificate is pinned, not trusted on sight.** It arrives the same way, and is
  **copied out of the entry directory** at enrolment, because the next apply would delete it.
  `icinga2 pki save-cert` — trust on first use — remains as a fallback, taken only when no pinned
  certificate was delivered, and logged with the fingerprint it accepted.
- **Enrolment is three `pki` calls, never `node setup`:** `pki new-cert` (key and CSR, explicit
  paths), then `pki request` against the parent with the ticket, the pinned certificate, and the
  target paths. Nothing is written outside the Supervisor's own directory.
- **The certificate on disk is the state; the marker is a hint.** Enrolment runs when there is no
  usable certificate — verified with `pki verify`, not by looking at a flag — or when it is inside
  its renewal window, or when the common name or the parent changed. A marker file records what was
  enrolled, so the ordinary start costs no subprocess.
- **Without a ticket, enrolment is still correct.** The CSR reaches the master's signing queue and
  waits for `icinga2 ca sign` there. The Agent stays unhealthy until it is signed, saying so.
- **An unreachable master is a wait, not a failure.** Health reports what is missing, the attempt
  backs off and repeats, and the daemon is not started — no crash loop, and the Client's own startup
  never blocks on somebody else's master.
- **Renewal is the daemon's own**, with the Supervisor as a start-time safety net only: a certificate
  past its renewal window is re-requested before the daemon starts, never while it runs.
- **Revocation stays on the master.** Removing the Supervisor removes its key material with the
  directory (ADR-0019); the master's `icinga2 ca remove` is the operator's, and the Supervisor says
  so when it is retired rather than pretending it cleaned up.

### Repacked vendor packages, one artifact built on the oldest glibc it must serve

We will produce Icinga 2 artifacts by **repacking the vendor packages into a normalised, link-free
tree**, built by `opamp-package-fetch`, and we will **bundle everything except glibc**. We will ship
**one Icinga 2 artifact per platform, built on the oldest glibc it must serve** — the distribution
family is not the criterion.

Bound by this decision:

- **One normalised layout per operating system**, so one `program_path` serves every distribution:

  ```
  icinga2-<version>/
    sbin/icinga2            lib/                 share/icinga2/include/
    plugins/                doc/copyright
  ```

  What is deliberately left out: `/etc/icinga2` (the fleet delivers configuration), the systemd unit
  and init script, `prepare-dirs` and `safe-reload` (they need the `nagios` user), and documentation
  beyond the copyright files.
- **`.tar.gz` on every platform**, including Windows: it is the only container that carries the
  executable bit, and the Client unpacks it everywhere (ADR-0030's rule, applied without the
  exception GLPI could take because upstream published a zip).
- **Libraries ride along, glibc does not.** The `NEEDED` closure minus glibc and the loader is copied
  into `lib/` with links dereferenced, and the Supervisor points `LD_LIBRARY_PATH` at it. Bundling a
  libc without its loader does not work, and with its loader would mean an `exec` indirection the
  program path cannot express.
- **The build host is the reach.** The glibc floor is a property of the build host, not of Icinga,
  and choosing that host is the operator's decision. The tool states its consequence: it **measures
  and prints the floor** from the vendor's own package before anything is uploaded, so coverage is
  known before a rollout rather than discovered host by host, and it refuses to build for a
  distribution the host is not.
- **The Set is named after the Agent type**, as every other agent's is. `opamp-package-fetch` has no
  `--package-name` flag for a second Set; it would have nothing to do.
- **Two Sets remain possible, without the tool knowing.** A deployment that genuinely needs a second
  artifact — a host too old for the common floor, a family whose vendor binary it must run for
  support reasons — creates that Set through the REST API under a name of its own and aims it with
  a Selector. Nothing in the Server or the Client ever requires the tool's help for that.
- **The Red Hat caveat is documented, not designed around.** The manual states that a Debian-built
  tree carries Debian's OpenSSL layout, so a check that uses the system trust store on a Red Hat
  host is the one thing to verify before relying on it.
- **A plan may name several sources.** `opamp-package-fetch`'s plan grows from one URL to a list, so
  `icinga2-bin` + `icinga2-common` (+ plugins) become one artifact; every source is verified before
  anything is repacked.
- **Checksums come from the repository index.** Icinga publishes no per-file digest sidecars and
  signs its repositories with GPG instead — but the `Packages` index carries a `SHA256` field per
  file, and that is what every source is verified against before anything is repacked. It is also
  where the artifact's **reach** comes from: the same stanza's `Depends: libc6 (>= …)` is the
  vendor's own statement of the oldest glibc this build runs on, printed before the rollout rather
  than discovered host by host.
- **Extraction shells out** — `dpkg-deb`, `rpm2cpio`/`cpio`, and an MSI extractor — following the
  precedent of ADR-0030's AppImage repack, which also executes and is restricted to the platform it
  works on. A missing helper is refused by name, never worked around, and the Dev Container gains
  the ones it lacks.
- **Repacking is redistribution.** The vendor copyright files travel in the tree.

### The Windows artifact is verified by its publisher

We will verify the Windows MSI by its **Authenticode signature, pinned to the publisher**, and
repack it only when that verification passes.

Bound by this decision:

- **The check is two conditions, both required**: the embedded signature verifies against the file's
  contents, and the signer's subject names the expected publisher — `O=Icinga GmbH` for Icinga's
  MSI. A file that is unsigned, altered, or signed by somebody else is refused by name, and nothing
  is unpacked.
- **The chain to a root is reported, not required.** A Linux build host has no Authenticode root
  store, and carrying one inside this tool would be key management nobody asked it to do. What the
  refusal cannot claim, it does not claim: the tool says the signature is valid and whose it is, and
  says that the issuing chain was not validated locally.
- **The expected publisher is part of the agent's own definition**, beside its repository — not a
  flag. A publisher an operator can pass on the command line is a check that argues with itself.
- **`osslsigncode` is the verifier**, shelled out to as the extraction helpers are (see the repack
  above), and refused by name when absent. The Dev Container gains it.
- **This says nothing about whether the artifact runs.** Whether a repacked Windows tree relocates
  without the MSI's own product registration is an open question, and it needs a Windows host.
  Verification is about the bytes being the vendor's; the recipe still states Windows as unproven
  until someone runs it there.

## Alternatives considered

### Supervision

- **A recipe under the `command` kind, as for GLPI.** The honest first try, and the reason a kind of
  its own exists: it can express the arguments, but not the directory preparation, not the pre-start
  gate on the certificate, and not the validation. Its failure mode is the bad one — a fleet
  reporting `APPLIED` for a configuration Icinga refused, and a crash loop while a host waits for a
  certificate. Rejected on evidence, not on taste.
- **Operator-written hook keys** (`pre_start_cmd`, `validate_cmd`) on the generic kind. Rejected for
  the reason ADR-0011 already rejected them: *"a key that is the mechanism"* moves the decision into
  every host's configuration file and makes the Supervisor's behaviour unreviewable.
- **Driving `systemctl` / `sc.exe`.** Rejected as ADR-0030 rejected it: no watchdog, no health gate,
  no bounded stop, and it supervises a process the Client did not start.
- **Requiring the host to carry a `nagios` user.** Would remove the `RunAsUser` arguments and add a
  provisioning step outside the fleet, on every host, for no gain: the Client's account already owns
  the directories in question.
- **Preflight by inspecting the artifact statically** (reading the required `GLIBC_` symbols out of
  the tree and comparing them against the host). Rejected in favour of running the program once:
  a start attempt is the definition of "does this run here", it catches a missing library as well as
  a too-old libc, and it produces the message the operator needs without this project maintaining a
  model of dynamic linking.

### Enrolment

- **`icinga2 node setup` for the whole bootstrap.** Convenient, and wrong here: one command instead of
  three, and it writes into `ConfigDir`, which the spike showed to be the one constant that is not
  reliably relocatable. Rejected on that alone; the `pki` subcommands do the same work with explicit
  paths.
- **Let the fleet Server issue Icinga certificates** by extending ADR-0013's CA. Rejected: it makes
  the fleet Server a trust root in the monitoring topology, requires the Icinga master to trust it,
  and doubles the blast radius of a Server compromise for a certificate the master could sign itself.
- **Deliver the finished certificate and private key as Configurations.** Simplest to implement and
  the worst outcome: a private key generated centrally, travelling through the Server, stored in its
  Configuration store, and readable to whoever reads the fleet. Rejected outright — the Icinga flow
  exists precisely so the key stays put.
- **Trust on first use as the default.** Convenient at scale, and it hands the first attacker in the
  path a permanent foothold. Kept only as a logged fallback for the case where nothing was pinned.
- **A ticket in `client.toml` on every host.** Works, and puts a per-host secret in the file the
  fleet also rewrites (ADR-0019). Rejected as the default: the Selector-targeted Configuration is
  the mechanism this project already has for exactly this.
- **Enrolment in `Plugin::start`** rather than in the adapter task. Rejected: it would block the
  Client's startup on the reachability of an Icinga master.

### The artifact

- **Build Icinga 2 from source with a prefix of our own.** The robust answer to relocation, and the
  one ADR-0030 already weighed for GLPI: *"a compile step per architecture and a dependency list that
  moves with every release"*. Rejected while repacking demonstrably works — and it stays the fallback
  if a platform turns out not to relocate.
- **Ship the vendor `.deb`/`.rpm`/MSI and install it on the host.** Standard paths, no relocation
  problem — and an installation beside the fleet, needing root, a package manager, and a service the
  Client does not supervise. That is the outcome this whole line of work exists to avoid.
- **Bundle glibc too, and ship the loader.** Would make one Linux artifact serve every distribution.
  Rejected: the program would have to be the loader, which the block's program path cannot express,
  and a mismatched loader/libc pair is a class of failure worse than a clear refusal.
- **`patchelf` the RUNPATH instead of setting `LD_LIBRARY_PATH`.** Unnecessary — RUNPATH loses to
  `LD_LIBRARY_PATH`, measured — and it would rewrite a vendor binary, which is a change to the thing
  whose checksum was just verified.
- **`.7z` or `.zip` for Windows.** Rejected for the reason ADR-0030 gives: they carry Windows
  attributes, so the tree would arrive without executable bits.
- **One artifact per distribution family, in two Linux Sets distinguished by name.** A Set carries
  one entry per `(os, arch)` ([ADR-0017](0017-package-artifacts.md)) and versions compare
  without build metadata
  ([ADR-0009](0009-version-baking-comparison-and-display.md)), so a
  Debian-built and an EL-built `linux/amd64` tree could not share one Set; they would be separate
  Sets of the same Agent type, aimed by a Selector at an attribute the operator sets on the Client.
  Conservative, and rejected on the evidence: it is stricter than the glibc constraint it is derived
  from, it doubles the build and test surface for no reachability gained, and for Red Hat it
  prescribes an artifact that cannot currently be fetched without a subscription.
- **One Set with a distro-suffixed version** (`2.14.6+el9`). Rejected: ADR-0009 compares without
  build metadata, so the two would compare equal and the offer would be a coin toss.
- **Ship the Red Hat artifact anyway, from the subscription repository.** Possible for an operator
  who has one, and a credential this tool would then have to carry. Rejected as the *default*: a
  single artifact already serves those hosts, so the subscription becomes a choice rather than a
  requirement.
- **Build on the newest distribution and require recent hosts.** Simplest to produce, and it quietly
  excludes exactly the hosts most likely to be running an old agent. Rejected — the floor should be
  a decision, not a side effect of what the build machine happened to be.

### The Windows artifact

- **Require an operator-supplied `--sha256`.** Rejected: it moves the trust decision to whoever types
  the command, with a digest they took from the same page as the file — and it makes the ordinary
  path a manual one, which is how verification comes to be skipped.
- **Trust the TLS connection to `packages.icinga.com` and repack.** Rejected outright: it would put
  the artifact's integrity in the hands of whoever serves that path, which is exactly what the
  verification rule exists to avoid.
- **Carry the GlobalSign code-signing root and validate the full chain.** Stronger on paper. Rejected
  for now: it pins this tool to one CA's roots and their rotation, and the publisher check already
  binds the artifact to a key an attacker on the mirror does not have. Revisit if a deployment needs
  a chain-verified provenance claim.
- **Verify on a Windows host instead**, where the chain validates. Rejected as a requirement: the
  repack itself runs on Linux (the MSI extracts there), and needing a second platform to check a
  download would make the Windows artifact harder to produce than to trust.
- **Skip Windows until it is proven to run.** Tempting, and it confuses two questions. Whether the
  bytes are the vendor's is answerable today; whether the tree relocates is not, and is recorded as
  open.

## Sources / Prior art

- Spike against **Icinga 2.14.6-1** (Debian trixie packages, 2026-08-17): relocated tree with
  bundled libraries, `strace`-verified absence of any compiled-in path access, the `-D` matrix, the
  `RunAsUser` refusal, the `-I` vs. `IncludeConfDir` measurement, pid stability across `SIGHUP`, the
  silent reload abort, `SIGTERM` within two seconds, `readelf -d` (RUNPATH), the `ldd` closure,
  `objdump -T` for the `GLIBC_` floor, and a 39 MB / 52-file tree. `pki new-cert` and `pki verify`
  with explicit paths touch no system directory, write the key `0600`, and require the same
  `-D RunAsUser=`/`-D RunAsGroup=` as every other invocation.
- Measured against the vendor repositories (2026-08-17): the `Depends: libc6 (>= …)` of
  `icinga2-bin` for bullseye, bookworm and trixie; the open EL repository ending at 8; and
  `packages.icinga.com/subscription/` answering `401`.
- Measured against `Icinga2-v2.16.4-x86_64.msi` (2026-08-17): no digest sidecar published; the
  embedded signature verifies with a timestamp, signer `O=Icinga GmbH`, issuer GlobalSign GCC R45
  CodeSigning CA 2020; chain validation fails on a Linux CA bundle only.
- glibc's symbol versioning, which is what makes "built old, runs new" true and its converse false.
- [Icinga 2 CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/) — the
  `daemon` flags, and `pki new-cert`, `save-cert`, `request`, `verify`, `ticket`, `ca sign`.
- [Icinga 2 language reference](https://icinga.com/docs/icinga-2/latest/doc/17-language-reference/) —
  the constants and the include semantics.
- [`icinga-app/icinga.cpp`](https://github.com/Icinga/icinga2/blob/master/icinga-app/icinga.cpp) —
  where the path constants come from per platform, and that `-D` is applied before they are frozen.
- [Icinga 2 — Distributed Monitoring](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/):
  the ticket as *"a client ticket … generated on the master"*, CSR auto-signing, and the on-demand
  signing queue.
- [packages.icinga.com](https://packages.icinga.com/) — the vendor repositories the artifacts come
  from, and their GPG-signed rather than digest-listed shape.
- [`osslsigncode`](https://github.com/mtrojnar/osslsigncode) — the OpenSSL-based Authenticode
  verifier, packaged by Debian; the same shell-out pattern the repack uses for `dpkg-deb` and
  `msiextract`.
- [ADR-0011](0011-supervisor-mode-core-and-plugins.md) (a kind is a module plus a registry
  line), [ADR-0011](0011-supervisor-mode-core-and-plugins.md) (the lifecycle vocabulary and the
  extension point), [ADR-0030](0030-glpi-agent-supervision-and-packages.md) (the
  foreground-daemon shape and what a Foreign Agent recipe looks like when no plugin is needed).
- [ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md) — the other PKI in this
  project, and the one this decision deliberately does not reuse.
- [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)
  — the delivery mechanism for a per-host file that is not the process's configuration.
- [ADR-0030](0030-glpi-agent-supervision-and-packages.md) — the repack precedent:
  dereferencing links, deterministic `.tar.gz`, verifying upstream's hash at packing time, and the
  container/mode rule.
- [ADR-0017](0017-package-artifacts.md), [ADR-0015](0015-package-delivery-for-managed-processes.md) and [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md)
  — the containers, the tree limits, one entry per platform in a Set (which is why two artifacts for
  one platform ever need two Sets), and the Set model this fits into.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), [ADR-0017](0017-package-artifacts.md)
  — the fleet's own model, where a package's content hash and Ed25519 signature protect what a
  Client installs; the Windows verification decides the *other* end, what the operator's tool is
  willing to repack.

## Consequences

### Supervision

- Positive: Icinga 2 becomes an ordinary fleet citizen — rolled out, updated, rolled back, and
  configured through the same acts as every other Managed Process, with no host-side installation.
- Positive: the nine relocation arguments are written once, in code, instead of once per host; the
  three that are not operator choices cannot be got wrong at all.
- Positive: the preflight makes every tree package safer, not only Icinga's — a Collector built
  against a newer libc is refused before the running one is stopped.
- Negative / trade-offs: a third plugin kind to maintain, and one that shells out to its own program
  for validation and enrolment. Accepted: the alternative is a Supervisor that lies about applies.
- Negative / trade-offs: two shared-code changes touch what the Collector and the `command` kind
  use. Both are additive and default to the existing behaviour.
- Negative / trade-offs: the Managed Process runs under the Client's service account, not `nagios`,
  so checks needing elevated capabilities (`check_icmp`) fail. Documented, not worked around.

### Enrolment

- Positive: a host enrols itself with no operator on it, and the fleet never becomes a certificate
  authority for a system it does not own.
- Positive: the private key is generated where it is used and never travels; the worst a compromised
  fleet Server leaks is a ticket bound to one common name.
- Negative / trade-offs: the fleet Server must be given per-host Configurations for tickets, which is
  a Configuration per enrolling host until it is signed. Selectors make that mechanical, and the
  ticket may be withdrawn afterwards.
- Negative / trade-offs: a host waiting for on-demand signing looks like an unhealthy Agent for as
  long as nobody signs. Correct, and it belongs in the manual's troubleshooting table so it is not
  reported as a defect.
- Negative / trade-offs: the Supervisor shells out to `icinga2 pki` and depends on its exit codes and
  output. Bounded by a timeout, and the alternative — reimplementing the CSR protocol — is worse.

### The artifact

- Positive: an Icinga 2 that installs nothing on the host, updates and rolls back like any other
  package, and carries its own libraries, ITL and check plugins.
- Positive: no build system for a C++ project with Boost and OpenSSL enters this repository.
- Positive: one artifact, one Set, one upload — and Red Hat hosts are served today rather than after
  a subscription is bought.
- Positive: the tool carries no flag for a second Set, and the manual no two-Set procedure nobody
  has to follow.
- Negative / trade-offs: the build host's glibc decides reach, so picking the build host is a real
  decision, with a floor that has to be chosen deliberately. The tool prints it; the manual states
  it per artifact and says to build on the oldest host you serve, rather than leaving it to be
  discovered on a host.
- Negative / trade-offs: the artifact carries the build distribution's OpenSSL layout onto hosts of
  another family. Inert for cluster TLS; documented for the checks where it is not.
- Negative / trade-offs: running a vendor binary on a family it was not built for is a support
  question this project cannot answer for an operator. Stated in the manual, not decided here.
- Negative / trade-offs: the tool grows extraction helpers it must shell out to, and therefore
  platform restrictions on where a repack can run.
- Negative / trade-offs: the build host must carry the vendor package's own dependencies, because
  the tree bundles what `ldd` resolves *there*. One that cannot be resolved is refused by name
  rather than packed around — a tree missing a library would otherwise ship and die on its first
  start.

### The Windows artifact

- Positive: the Windows artifact is bound to Icinga's signing key rather than to a mirror's honesty
  — a stronger claim than the Linux path's digest, from a source that publishes less.
- Positive: the question generalises, so the next artifact without an index has an answer already.
- Negative / trade-offs: another shelled-out helper, and one more thing a build host must carry.
- Negative / trade-offs: the chain is unvalidated locally, so what is proved is "signed by a key
  whose certificate says Icinga GmbH" rather than "signed by a certificate a trusted root vouches
  for today". Stated in the output rather than glossed.
- Negative / trade-offs: a publisher rename, or a signing certificate issued to a differently spelled
  subject, breaks the build until this project is updated. Accepted: a pin that never fails is not a
  pin.

### Follow-ups

- The RPM path stays unimplemented, and it is optional rather than required — its `repomd` index is
  the equivalent source of digests, and what would revive it is a deployment that must run the
  vendor's own Red Hat binary; that decision brings the subscription credentials with it.
- Windows is unproven: whether the repacked Windows tree runs relocated needs a Windows host.
  If the MSI payload cannot be relocated, the same kind supervises a machine-installed Icinga 2
  there, which is why the absolute-path form of `binary` is in scope from the start.
- Chain validation against a carried root, if provenance ever has to be provable rather than merely
  bound.
- Renewal is left to the daemon and only netted at start; if that turns out not to hold in practice,
  an explicit renewal schedule is its own decision.
