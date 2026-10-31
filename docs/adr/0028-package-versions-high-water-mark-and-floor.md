# ADR-0028: Package versions, the high-water mark, and the floor

- **Status:** 🟢 accepted
- **Date:** 2026-10-26
- **Deciders:** Markus Brigl
- **Applies to:** the version rule of `crates/fleet-core/src/version.rs` and its callers, the floor statement in `crates/fleet-core/src/package.rs`, the Package identity and the store's reading of it in `crates/fleet-server/src/packages.rs`, `crates/fleet-server/src/fs/packages.rs` and `crates/fleet-server/src/migrate.rs`, the candidate test of `crates/fleet-server/src/packages.rs`, the package and `/api/v1/package-floors` routes of `crates/fleet-server/src/api.rs`, the floors' store in `crates/fleet-server/src/fs/` and `crates/fleet-server/src/postgres/`, the layout of `data_dir` in `crates/fleet-server/src/fs/mod.rs`, the `/v1/package-floors` route on the Agent plane in `crates/fleet-server/src/transport.rs` and downstream of a Gateway in `crates/fleet-agent/src/gateway/`, the command trees of `opamp-fleetctl` and its `package sign`, `package fetch` and `package floor` commands in `crates/fleet-tools/`, the Supervisor's check of a package offer in `crates/fleet-agent/src/supervisor/agent/packages.rs`, the self-update's check of an offered version in `crates/fleet-agent/src/update/`, the installed-package record in `crates/fleet-agent/src/storage.rs`, the Client's fetch of the floors, the files `package-high-water.json` and `package-floors.json` under the Client's `state_dir`, the purge in `crates/fleet-agent/src/reconfigure.rs`, and every document that tells an operator how a Package is numbered, how an old signed build is kept off the fleet, or how a Supervisor is moved to an older version

## Context

A package signature covers the Agent type, the version and the artifact's SHA-256
(`fleet_core::package::statement`,
[ADR-0027](0027-packages-signatures-and-delivery.md) clause 52). It binds an artifact to its own
version, but it says nothing about how that version orders against another, and it never
expires. A build the operator signed once stays validly signed forever, including a build with a
flaw that became known later. Whatever stops a downgrade must therefore be a rule about versions,
and it must hold against the Server, because the attack comes from the Server. The specification
puts security before convenience; **Q-1** asks that no package installs without a valid
signature, and **Q-6** that no Client installs a Package below the highest version it has
installed for that Agent type, or below the highest floor the operator signed for it. **G-10** and
**G-11** ask that the Server update an agent's software and the Client's own, verified before it is
applied. The threat model trusts the Server to manage the fleet but not with what a host must
verify itself: a compromised Server must not get a host to run an older signed build.

The forces:

- **The Server manages the Supervisor set.** A Supervisor removed from the set is purged, its
  directory deleted, installed-package record included
  ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)). A floor kept in the Supervisor's directory
  is one a compromised Server can reset by sending a set without the Supervisor and then one that
  adds it back, under the same name or another.
- **A version that does not compare cannot be refused by order.** `precedence` in
  `crates/fleet-core/src/version.rs` returns `None` for any value that is not SemVer. If an
  operator ever signed a build under a version such as `1.19`, which some upstream agents' releases
  use, an ordering test that lets an incomparable version through lets the Server offer that build
  over any installed version, however new.
- **The Client's own update has a floor no Server act lowers:** the binary that is running
  ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)).
- **An honest Server does not move an Agent backwards.** A rollout reaches an Agent only when it
  moves the Agent forward, rollback is not a rollout, and a deliberate Server-driven downgrade is
  left undecided ([ADR-0026](0026-deployments-and-rollouts.md)). That rule binds an honest Server;
  the refusal that counts is the Client's.
- **The one move to an older version takes no offer.** When an apply fails within the apply grace,
  the Supervisor renames the predecessor back over the live program
  ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 59).
- **A mark knows only what this host installed.** An old build the operator signed under a version
  above the current mark, such as a long-ago pre-release of a later version (`2.0.0-rc.1` while
  the fleet runs `1.9.4`), is taken by a host that never ran anything newer. What closes that gap
  is a statement only the operator can make: below this version, nothing of this Agent type is
  taken any more. It must reach every Client, through Gateways too, and a Server that cannot sign
  must not be able to lower it.

## Decision

We will require every Package version to be SemVer 2.0.0, on the Server, in `opamp-fleetctl` and
on the Client, and have every Client refuse a package of an Agent type below the highest version
it has installed for that type, kept under `state_dir` where no purge reaches it, or below the
highest floor the operator signed for that type.

### Versions

1. **A Package is identified by `(agent_type, version)` and nothing else, and the version is
   SemVer.** Both are tokens of 1–64 characters from letters, digits, `.`, `_`, `+` and `-`, no
   `@`, no path separator. The version is, beyond that, a SemVer 2.0.0 version (clause 2). A
   request that would create a Package under any other version is answered `400`, with a text
   that names the version and says a Package version is SemVer, for example `1.19.0` for
   upstream's `1.19`. A Package that the store already
   holds under a version that is a token but not SemVer is read as clause 3 says; it does not fail
   the store's opening ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 8). A Package
   has no name of its own, no Selector and no kind. Its entries carry their signatures
   ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 18). Creating a Package that exists
   is the same request arriving twice.

2. **One predicate decides what is SemVer, in the internal crate.** A version is SemVer when
   `fleet_core::version::parse` accepts it ([ADR-0033](0033-the-products-version.md)) and no
   numeric pre-release identifier has a leading zero. The Server, `opamp-fleetctl` and the Client
   all call this predicate, and none of them has a second copy. Two versions are ordered by
   `precedence`, build metadata ignored. Two builds of one release compare equal.

3. **A Package with a non-SemVer version that the store holds is kept, and nothing new is done with
   it.** At startup the store reads it, and the Server logs one warning per such Package, naming it
   and the SemVer rule. Such a Package is never a candidate (clause 5). Every write to it is
   answered `400` with the same text as clause 1: an entry, a signature, and adding it to a
   Deployment. It may be deleted like any other Package once nothing pins it
   ([ADR-0026](0026-deployments-and-rollouts.md), deleting what an assignment pins). An assignment
   that already pins it stays in force, and its offer still goes out
   ([ADR-0026](0026-deployments-and-rollouts.md), the pin of what an Agent installed). The Client
   refuses that offer (clause 7), and the fleet view shows the reason the Agent reports.
   `server migrate` carries such a Package over as it reads it. The operator republishes the
   content under a SemVer version, signs it again, and rolls that out.

4. **`opamp-fleetctl package sign` signs only a SemVer version.** It refuses a `--version`, and a
   signing-file entry ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 20), whose
   version is not SemVer. It does so before it reads the key, and names the version. The
   operator's key therefore never signs a version that a Client would refuse.

5. **A Package becomes a candidate when its version is SemVer, it fits, it is aimed at the Agent,
   and it moves the Agent forward.** A Package whose version is not SemVer (clause 3) is nobody's
   candidate, whatever the Agent reports. Fit is
   [ADR-0027](0027-packages-signatures-and-delivery.md) clause 14's, mandatory, and comes first
   after that. The version test ([ADR-0026](0026-deployments-and-rollouts.md)) runs *with* the
   fit: a Package it holds back is nobody's candidate and appears in no count and no proposal. A
   per-Agent refusal of a non-SemVer Package names the version and the SemVer rule, as the
   version reasoning of a refused per-Agent act does
   ([ADR-0026](0026-deployments-and-rollouts.md)).

6. **A Package is numbered in the space its program numbers itself, written as SemVer.** For a
   program that self-reports an orderable version (the Client and any OpAMP-aware Managed
   Process), a Package numbered below it can never reach it. An operator who numbers a Package by
   hand takes the program's own number. A number with fewer than three numeric components is
   completed with `.0` (`1.19` becomes `1.19.0`), and a leading zero is dropped from a numeric
   component (`24.04.1` becomes `24.4.1`). Both keep the program's own order. A number that cannot
   be written as SemVer this way gets a SemVer version that the operator chooses in the same
   order. `opamp-fleetctl package fetch` names a Package by this rule. It fetches the release
   under upstream's own tag, and it refuses an upstream number it cannot write this way, naming
   it.

### The high-water mark

7. **A Supervisor takes its own type, only a SemVer version, and never goes back.** Each of the
   following offers is refused before anything is downloaded:
   - an offered package whose name is not the Supervisor's Agent type;
   - one whose version is not SemVer (clause 2);
   - one whose version precedes the installed one by SemVer precedence;
   - one whose version precedes the high-water mark of the Supervisor's Agent type (clause 8).

   An offer equal to either is not refused on that ground. Every refusal is reported in
   `PackageStatuses.error_message`, naming the offered version and the version it was compared
   against. The Client's own update keeps its own rules (clause 21).

8. **The high-water mark is the highest version this Client has installed for each Agent type.**
   It lives in `<state_dir>/package-high-water.json`, as a JSON object that maps an Agent type to a
   version. The file is written owner-only, through a temporary file and a rename. It is keyed by
   the Agent type, not by Supervisor name. The signature binds an artifact to its Agent type, and a
   Supervisor takes only its own type (clause 7), so the type is the one key that a Server cannot
   change without a signature the operator never made. A name can be changed by re-adding the
   Supervisor under another one. Every Supervisor of one type on this Client shares the type's
   mark. The file sits outside `supervisor_dir`, and its name contains a `.`, which no Supervisor
   name can contain (the instance-name grammar of
   [ADR-0019](0019-the-installed-service-and-its-state.md)), so no purge can delete it, even where
   an operator places `supervisor_dir` at `state_dir` itself. The mark rises when a Supervisor
   reports a package `Installed`, that is, once its program has survived the apply grace
   ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 58). It never falls. It does not
   move on `InstallFailed`, and it does not move on the rollback of
   [ADR-0027](0027-packages-signatures-and-delivery.md) clause 59.

9. **The mark is seeded at every start from what is installed.** At startup, each Supervisor's
   installed-package record with a SemVer version raises its Agent type's mark to at least that
   version. So a Client whose `state_dir` holds no mark file starts with a mark that matches what
   its Supervisors already run, before its first connection; the same holds for a file that was
   deleted. If a write of the file fails, the Client logs a warning and keeps the raised mark in
   memory. The next start seeds the mark again from the record.

10. **An installed-package record whose version is not SemVer is dropped at load.** The Client logs
    a warning that names the Supervisor and the version, and deletes the record. Nothing enters
    the mark from it. The program in `program/` keeps running. The Agent then reports no
    `agent_has_version` for the package, so a rollout of the SemVer republish reaches it as a first
    rollout ([ADR-0026](0026-deployments-and-rollouts.md)).

11. **A mark file that cannot be read refuses every Supervisor package.** A file that exists but
    does not parse, or that maps a type to a value that is not SemVer, is never read as empty.
    Startup logs an error naming the file. Every package offer to a Supervisor is then refused and
    reported with a reason that names the file, until the operator repairs or deletes it. The
    Client keeps its connection, its configurations and its own self-update.

12. **Only the host lowers the mark.** No Server route, no offer, no delivered Supervisor block and
    no key of `supervisor.toml` reads or writes the file. An operator who wants a Supervisor on an
    older version stops the Client and deletes the file, or removes that type's entry from it. The
    operator also deletes `installed-package.json` of every Supervisor of that type, because
    clause 9 seeds the mark again from those records. Then the operator starts the Client.
    Deleting the whole file resets the marks of every type that no remaining record names. The
    recommended path needs no host access: publish the old content as a new, greater version
    ([ADR-0026](0026-deployments-and-rollouts.md)).

13. **A removed Supervisor is purged, and its Agent type's high-water mark stays.** Removal is keyed
    by name: only a name absent from the new set is removed, while a changed block keeps its
    directory, identity and installed package. The purge runs only after the write succeeded,
    because a failed write restarts the old set from whole directories. It deletes
    `<supervisor_dir>/<name>/` recursively: program, staging, configuration, installed-package
    record and `instance-uid`. A Supervisor later re-added, under the same name or another, is a
    new Agent, installed afresh. The purge does not touch `<state_dir>/package-high-water.json`
    (clause 8), so whatever the re-added Supervisor installs is held to the mark of its Agent type.

### The floor

14. **A floor is a signed statement of an Agent type and a version.** The statement is
    `opamp-fleet-floor-v1`, the Agent type and the version, one per line, each ended by a newline
    (`fleet_core::package::floor_statement`), signed with the operator's Ed25519 package key. The
    version is SemVer by clause 2. The prefix differs from the package statement's
    `opamp-fleet-package-v1`, so no package signature verifies as a floor and no floor as a
    package. `opamp-fleetctl package floor --agent-type … --version …` signs it and prints the
    signature in hex, as `package sign` does.

15. **The Server stores one floor per Agent type and serves them to every member.** On the Operator
    plane `PUT /api/v1/package-floors/{agent_type}` with the version and the hex signature stores
    it, `GET /api/v1/package-floors` lists them, and `DELETE /api/v1/package-floors/{agent_type}`
    removes one. A `PUT` whose version SemVer orders below the stored one for that type is `409`; a
    floor is raised, or removed and set anew. Where the Server holds `[packages] verification_key`,
    it checks each floor when it is written and reports it valid or invalid, as it does for a
    package signature: feedback only, since the Client's check decides. The floors are stored as
    the Server stores its other records, in its data directory or in PostgreSQL
    ([ADR-0029](0029-the-servers-state-and-its-stores.md)), in the data directory as the file
    clause 23 adds to its layout. Every action is audited. On the Agent plane
    `GET /v1/package-floors` answers every admitted member with all floors as a JSON object that
    maps an Agent type to its version and signature, behind the admission of `/v1/opamp`.

16. **A Gateway serves the floors it holds to the hosts behind it.** A Gateway fetches the floors
    from its Server when it starts and then once an hour, and answers `GET /v1/package-floors` on
    its downstream listener with what it last fetched, behind the handshake and the revocation
    verdict of its `/v1/opamp`; an enrolling peer is refused them. It checks no signature on their
    behalf; each host checks its own. A host behind a Gateway therefore takes a floor only after
    the Gateway's next fetch, and keeps the old ones while the Gateway's fetches fail.

17. **A Client keeps the highest verified floor of each Agent type.** It fetches the floors from
    its endpoint after its first reply, then once an hour while it is connected, and before it
    decides a package offer when its last fetch is more than a minute old. It verifies each against
    `[packages] verification_key` and keeps, per Agent type, the highest version that verifies, in
    `<state_dir>/package-floors.json`, a JSON object that maps an Agent type to a version and the
    signature it came with, written owner-only through a temporary file and a rename. A floor that
    does not verify, or that orders below the one kept, is ignored and logged. A failed fetch keeps
    the floors held and is logged, and an offer is decided against them. The file lies outside
    `supervisor_dir` and its name holds a `.`, so no purge reaches it (clause 8). A floor is
    verified when it is kept, not again when the file is read, so a later change of the key does
    not undo it. A file that exists but does not parse, or maps a type to a value that is not
    SemVer, is never read as empty: every package offer, the Client's own included, is refused and
    reported with a reason that names the file, until the operator repairs or deletes it.

18. **No package below its type's floor is taken.** A Supervisor refuses an offer whose version
    SemVer orders below the floor kept for its Agent type, before anything is downloaded, and
    reports it `InstallFailed` with a reason that names the version and the floor. The check stands
    beside the high-water mark and the installed record of clause 7; the stricter one decides. An
    offer of the package the Supervisor already runs is no install: it is answered as installed,
    below the floor too (clause 19).

19. **What is installed stays.** A Managed Process whose installed version lies below its type's
    floor keeps running, and so does the Client. A Supervisor reports in its Agent's health that it
    runs below the floor, naming both versions, and takes the next offer at or above the floor. The
    Client's own Agent reports no such health: its version is the running build's, and its next
    update at or above the floor replaces it. The rollback of
    [ADR-0027](0027-packages-signatures-and-delivery.md) clause 59 puts back the installed
    predecessor whatever the floor says, since it takes no offer, and the health then says so too.

20. **Only the host lowers a floor.** No Server route, no offer, no delivered Supervisor block and
    no key of `supervisor.toml` lowers what `package-floors.json` holds. An operator who wants a
    Client below a floor stops the Client and removes that type's entry from the file, or deletes
    the file, and starts the Client. The Server's `DELETE` only stops serving a floor to the
    Clients that have not kept it.

21. **The Client's own floor is the higher of its running build and the floor of its Agent type.**
    It refuses an offered version that is not SemVer (clause 2), one that orders below the running
    build ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)), and one that
    orders below the floor kept for its Agent type. It keeps no high-water mark: the Client's
    Agent type is not a key in `package-high-water.json`. The crash-loop pointer move back to the
    previous version is local, takes no offer, and stays as
    [ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md) decides it.

### The floor in the operator program and the data directory

22. **One program, two command trees.** `opamp-fleetctl package` holds `fetch`, `pack`, `sha256`,
    `sign`, `keygen` and `public-key`, each with the options and output it has, and `floor` as
    clause 14 states it. `opamp-fleetctl pki` holds `init`, `server-cert`, `bootstrap-ca`,
    `status` and `p12-extract` as [ADR-0032](0032-the-operator-program-opamp-fleetctl.md) states
    them.
    `opamp-fleetctl --version` prints the version every binary of this project prints
    ([ADR-0033](0033-the-products-version.md)). No other program name exists for any of these
    commands: no alias, no wrapper, no symlink.

23. **The layout of `data_dir` is fixed, and no other is read.** With `store = "fs"`, under
    `data_dir`:

    | Path | One file per | Domain |
    |---|---|---|
    | `configurations/<name>.json` | Configuration, with its retained revisions | Configurations ([ADR-0026](0026-deployments-and-rollouts.md)) |
    | `deployments/<name>.json` | Deployment | Deployments |
    | `packages/<agent_type>@<version>.json` | Package: identity and entries, no bytes | Packages ([ADR-0027](0027-packages-signatures-and-delivery.md)) |
    | `blobs/<sha256>` | artifact, with the filesystem blob store | Blobs ([ADR-0029](0029-the-servers-state-and-its-stores.md)) |
    | `agents/<instance_uid>.json` | Agent: report, assignment, labels | Fleet ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)) |
    | `hosts/<host>.json` | Host | Hosts |
    | `certificates/<issuer>-<serial>.json` | certificate the Server signed | Certificate register |
    | `revocations.json` | the revocation list | Revocations |
    | `bootstrap/<issuer>-<serial>.json`, `.p12` | bootstrap certificate the Server issued | Bootstrap |
    | `enrolment.json` | the window and its pending requests | Enrolment ([ADR-0029](0029-the-servers-state-and-its-stores.md), the enrolment window) |
    | `package-floors.json` | the floor of every Agent type | Package floors (clause 15) |
    | `audit/audit-<first seq>.jsonl` | audit file | Audit |

    With `store = "postgres"`, `data_dir` holds `audit/` and, with the filesystem blob store,
    `blobs/`, and nothing else. Beside a bucket, a `blobs/` an earlier filesystem blob store left
    is accepted too, as the place the switch from the filesystem to S3 copies from
    ([ADR-0029](0029-the-servers-state-and-its-stores.md)); the Server does not create it there.

    `<issuer>` is the first 16 hex characters of the SHA-256 of the issuer's DER name, and
    `<serial>` is the serial in the register's lowercase hex form. A file or directory the layout
    does not name, a file that does not parse, or a file whose name disagrees with its content
    fails startup, naming the path. A path of the filesystem layout found under a `data_dir` used
    with `"postgres"` fails startup naming it and `server migrate-to-postgres`. The one exception
    is a temporary file the Server's own write left behind, a regular file whose name ends in
    `.tmp`, whose rename never happened: it is removed at startup, since the record beside it is
    the one before the write.

**Out of scope:** a deliberate downgrade driven by the Server, which
[ADR-0026](0026-deployments-and-rollouts.md) leaves undecided; a list of withdrawn builds by
hash; an expiry or a signing time in the package statement; rotating the package key; a floor per
Platform or per Deployment; the floors in the bundled UI, beyond what the REST API serves; a
Server that withholds a floor, which delays it until the Client reaches one that serves it;
raising the mark from a version that a Managed Process reports about itself, as opposed to one
this Client installed; an operator command that lowers a mark; reporting the mark to the Server;
the Gateway refusing to cache a non-SemVer offer, which the Client's refusal decides, as the
Client's signature check decides the rest
([ADR-0027](0027-packages-signatures-and-delivery.md) clause 19).

## Alternatives considered

- **SemVer only, without a mark.** This closes the `1.19` path. It leaves the purge path open,
  because after a purge and a re-add there is no installed record to compare against, and any
  signed SemVer build of the type is taken. Rejected.
- **A mark per Supervisor name.** A Server that manages the set chooses the name, so it re-adds the
  Supervisor under a new one, and the new name has no mark. Rejected.
- **The mark inside the Supervisor's directory**, the `installed-package.json` record. The purge
  deletes it, and that is the attack. Rejected.
- **Let the purge keep `installed-package.json`.** This is still keyed by name and still bypassed
  by a re-add under another name. It also leaves a record about a program that is gone. Rejected.
- **The installed version alone as the floor, with versions that do not compare taken.** The
  signature still binds such a version, but an operator's `1.19` would be offerable over any
  installed version, and a Supervisor after a purge would take any version. Rejected for the
  rule of clause 7.
- **The running build alone as the Client's own floor, any offered value that is a version
  taken.** A build the operator once signed under a version above the running one, such as a
  long-ago pre-release of a later version, stays installable on every host that never ran it, and
  a Server that is compromised can offer it. Rejected for the floor of its Agent type beside the
  running build, and for SemVer as the test of a version.
- **A minimum version inside the signed package statement.** Each new signature would name the
  oldest version a host may hold afterwards. An old build carries its old signature, which states
  the old minimum. The floor would only take effect once a host had installed something newer,
  and it would need its own persisted store, which is clause 8 again. It would also change the
  signing format and every existing signature. Rejected.
- **A signed floor document with freshness, as in TUF's snapshot and timestamp roles.** It would
  also defeat a Server that withholds a newer floor, but it needs a second key role and a
  timestamp the Client checks. The floor of clause 14 is one statement per type under the one
  package key; withholding only delays it (Out of scope).
- **Server side only: refuse non-SemVer versions and track a floor per host on the Server.** A
  compromised Server is the threat, and it would not enforce its own floor. Rejected as the only
  defence. The Server-side refusal of clause 1 stays, as feedback to the operator.
- **Refuse a non-SemVer offer on the Client only.** This is safe, but the Server would still create
  Packages that no Client takes, and the operator would learn this only from the Agents' refusals.
  Rejected in favour of refusing on both ends.
- **Fail the Server's startup on a stored non-SemVer Package.** This is loud, but it leaves the
  operator with a Server that does not start, and with assignments that pin a Package nothing can
  delete while the Server is down. Keeping the Package read and inert (clause 3) is just as safe,
  because the Client refuses it. Rejected.
- **Rename a stored `1.19` to `1.19.0`.** The signature covers the version string, so a rename
  voids every signature on the Package. Rejected.
- **A signed list of withdrawn builds by hash.** It is exact, but the operator must know and name
  every bad build, and the list only grows. A floor names one version and covers every build below
  it, known or not. Not taken; it stays possible beside a floor.
- **A signing time in every package statement and a signed cut-off date.** It would also catch
  builds whose version nobody remembers, but it changes the package statement
  ([ADR-0027](0027-packages-signatures-and-delivery.md) clause 52), and every signature the fleet
  holds would have to be made again. Rejected.
- **Rotating the package key.** It removes every old signature at once, and with it every good one.
  It is a fleet-wide operation with no counterpart in the Client. Rejected as the way to withdraw
  one build.
- **Carrying the floor in the package offer**, as an extra entry of `packages_available` under a
  reserved name. It needs no route, but it gives a protocol field a second meaning that another
  OpAMP implementation would read as a package. Rejected.
- **A `CustomMessage`.** It is `[Development]` in the Baseline and not implemented here
  ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md) rejects it for the revocation
  list for the same reason). Rejected.
- **A floor file on the host alone.** It needs no Server, but every host would need configuration
  management the fleet does not have. Rejected.
- **A floor the Server may lower by a later signed statement with a sequence number.** A floor that
  only rises cannot be turned back by a compromised Server replaying an old statement, and the host
  can still lower it. Rejected.
- **Stopping a Managed Process below the floor.** A floor set too high would stop every Agent of
  its type across the fleet. Rejected; the health names the case instead.
- **Refusing every offer while the floors cannot be fetched.** A Server or Gateway that is briefly
  unreachable would stop every update. Rejected; the kept floors decide.

## Sources / Prior art

- The code read for the version rule and the mark:
  - `crates/fleet-agent/src/supervisor/agent/packages.rs`, the Supervisor's version check of an
    offer, against `installed_package`.
  - The installed-package record in `installed-package.json` in the Supervisor's own directory
    (`crates/fleet-agent/src/storage.rs`; `crates/fleet-agent/src/supervisor/mod.rs`).
  - `crates/fleet-core/src/version.rs` (`precedence`), which returns `None` when either side does
    not parse.
  - `crates/fleet-server/src/packages.rs` (`validate_identity_token`, `PackageId::new`), the one
    gate every write and every load goes through.
  - `crates/fleet-agent/src/reconfigure.rs` (`purge_removed`), which deletes the whole Supervisor
    directory.
  - `crates/fleet-agent/src/update/installer.rs`, the self-update's refusal of a value that does
    not parse and of a lower version.
  - `crates/fleet-agent/src/supervisor/process.rs` (`is_semver`), the leading-zero rule of SemVer
    item 9 that `parse` does not hold.
  - `crates/fleet-core/src/package.rs`, the signed statement.
  - `crates/fleet-agent/src/config.rs`, the Supervisor name grammar, which has no `.`.
- Semantic Versioning 2.0.0: item 2, a normal version is `X.Y.Z` of non-negative integers without
  leading zeros; item 9, a numeric pre-release identifier has no leading zero; item 10, build
  metadata is ignored in precedence; item 11, precedence compares major, minor and patch, then
  pre-release, and a pre-release ranks below its release. Recalled, not re-read for this record.
- The Update Framework (TUF) specification, the client workflow and the *rollback* and *freeze*
  attacks it names: a repository that serves validly signed but outdated metadata or packages,
  which TUF answers with version numbers that only rise and a signed timestamp. Recalled, not
  re-read.
- Android Verified Boot, *rollback protection*: a rollback index kept on the device, in
  tamper-evident storage outside the image it protects, that only rises, below which no signed
  image boots. It is the same split as clause 8: the floor does not live where the attacker can
  reset it. Recalled, not re-read.
- Mozilla's add-on and Chrome's extension blocklists, as the alternative of withdrawing builds by
  identity. Recalled, not re-read; cited for their shape, not for detail.
- Google Update's `RollbackToTargetVersion`: with the policy unset, a version higher than the one
  available is left alone.
- `docs/HARDENING.md`, its list of what a compromised Server cannot do.

## Consequences

- Positive: a compromised Server cannot move a Supervisor's Agent type below the highest
  version this Client has installed for it. Neither purging and re-adding the Supervisor nor a
  non-SemVer label gets past the refusal. The Supervisor path and the self-update path follow the
  same version rules.
- Positive: an operator can keep any build below a version off the whole fleet with one signature,
  without knowing which builds exist, the Client's own updates included. A compromised Server can
  neither forge nor lower a floor, so an old build signed under a version above a host's mark is
  refused once its type's floor is above it.
- Negative / trade-offs:
  - **Residual:** an old build signed under a version above both the mark and the floor is still
    taken. An operator who does not want such a build reachable raises the floor above it or
    deletes its Package.
  - **A mistaken high version sticks.** If a host installs a Package numbered by mistake, such as
    `100.0.0`, it refuses everything below that until an operator lowers the mark on that host
    (clause 12). The way back without host access is to publish under a version above the
    mistake. A floor set too high cannot be taken back from the Server either: each host is
    lowered by hand, or the operator publishes the old content as a new, greater version.
  - **Two Supervisors of one type share a floor.** On one Client, a new Supervisor of a type cannot
    be started on an older version than another Supervisor of that type has had.
  - **The mark and the floors outlive a reinstall.** `service uninstall` keeps `state_dir`
    ([ADR-0019](0019-the-installed-service-and-its-state.md)), so reinstalling the Client keeps
    them. Removing `state_dir` removes them, together with the Client's identity.
  - **A floor can be withheld.** It takes effect on a host only once a Server or Gateway that
    serves it has been reached, and behind a Gateway only after the Gateway's next fetch.
  - One more signed statement, store, route and state file. Every Client asks for the floors once
    an hour, and before an offer at most once a minute. The floors' store is part of the
    PostgreSQL schema; with the filesystem store it is a file the layout of an earlier Server does
    not name, so that Server refuses to start on the data directory until the file is moved away.
  - **Non-SemVer content:** a Server with a stored non-SemVer Package starts, warns about each
    one, and offers it only where an assignment already pins it; a Client refuses those offers.
    The operator republishes such content under a SemVer version (`1.19` becomes `1.19.0`), signs
    it again and rolls it out; an Agent whose record named the old version reaches that rollout as
    a first rollout (clause 10). A script that creates Packages under non-SemVer versions gets
    `400`. The changelog states all three.
- Follow-ups: the floors in the bundled UI; a list of withdrawn builds beside the floor; rotating
  the package key; whether a Managed Process's self-reported version should raise the mark; an
  operator command that lowers a mark on a host.

## Enforcement

Tests that carry `Verifies:` for this decision:

- **fleet-core:** the SemVer predicate takes `1.2.3`, `1.19.0` and `1.2.3-rc.1+abc`. It refuses
  `1.19`, `24.04.1`, `v1.2.3`, `1.0.0-01`, `nightly` and a value of 65 characters. The floor
  statement is the three lines of clause 14.
- **Server routes:** `PUT /api/v1/packages/otelcol/1.19` is answered `400` with a text naming
  SemVer, and `PUT /api/v1/packages/otelcol/1.19.0` creates the Package.
- **Server store:** a store holding a Package `otelcol@1.19` opens and warns, naming the Package.
  That Package is no candidate for an Agent that reports nothing. An entry write, a signature
  write and adding it to a Deployment are each answered `400`. Its `DELETE` succeeds while nothing
  pins it. An Agent record whose assignment pins it loads, and that Agent is still offered the
  Package.
- **Server floors:** `PUT`, `GET` and `DELETE` of `/api/v1/package-floors` in both stores, a `PUT`
  below the stored floor answered `409`, the check against `[packages] verification_key` reported
  but never refused, a package signature reported as no floor, every change audited, the floors'
  file part of the `data_dir` layout (clause 23), and `GET /v1/package-floors` admitted as
  `/v1/opamp` is, refused to a revoked certificate and to an enrolling host.
- **Server layout:** a `data_dir` holding `agents/` used with `store = "postgres"` fails startup
  naming `server migrate-to-postgres` (clause 23).
- **opamp-fleetctl:** `package sign --version 1.19`, and a signing file with an entry at `1.19`,
  each fail naming the version, without reading the key file. `package fetch` turns upstream
  `1.19` into the Package version `1.19.0` and fetches tag `1.19`. The `package` tree names
  `floor`, and `opamp-fleetctl pki --help` lists `p12-extract` among exactly the commands clause
  22 names.
- **Client, own type and older versions:**
  `a_package_for_another_type_or_an_older_version_is_refused` in
  [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs) (clause 7).
- **Client, non-SemVer offer:** a Supervisor with no installed record and no mark refuses an offer
  at `1.19`. Nothing is downloaded, and `error_message` names the version.
- **Client, purge and re-add:** a Supervisor `a` of type `otelcol` installs `0.110.0`. A set
  without `a` purges it. A set that adds `b` of type `otelcol` follows, and so does one that adds
  `a` again. Each re-added Supervisor refuses `0.109.0`, naming the mark, and takes
  `0.110.0+other` and `0.111.0`.
- **Client, when the mark rises:** a failed apply that rolls back to the predecessor leaves the
  mark where it was, and the predecessor runs. An `Installed` raises the mark and writes it
  owner-only.
- **Client, seeding:** with no mark file and an installed record at `0.110.0`, an offer at
  `0.109.0` is refused after a restart.
- **Client, unreadable mark file:** with a mark file that does not parse, every Supervisor offer
  is refused, naming the file, and the Client still connects.
- **Client, lowering the mark on the host:** with the mark entry and the type's installed records
  deleted while the Client is stopped, an offer at `0.109.0` is taken.
- **Client, old record:** an installed record at `1.19` is dropped at load with a warning, and the
  Agent reports no `agent_has_version`.
- **Client, floors**, in `crates/fleet-agent/src/`: a floor that does not verify, a package
  signature among them, or that is lower than the kept one, is ignored; a failed fetch keeps the
  kept floors; an unreadable `package-floors.json` refuses every offer, the Client's own included;
  a Supervisor refuses an offer below its type's floor before downloading and reports
  `InstallFailed`, and answers an offer of what it runs as installed; a Managed Process below the
  floor keeps running and its health says so; a rollback to a predecessor below the floor still
  happens; the self-update refuses a version below its type's floor.
- **Gateway floors**, in `crates/fleet-agent/tests/`: a host behind a Gateway receives the floors
  the Gateway last fetched, behind the downstream admission, and an enrolling peer is refused them.
- **Self-update:** `install_refuses_a_downgrade` and
  `the_probe_refuses_an_offer_that_is_not_a_version` in `crates/fleet-agent/src/update/installer.rs`
  (clause 21).
