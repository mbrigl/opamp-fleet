# ADR-0061: The operator signs a floor per Agent type, and no Client takes a package below the highest floor it has seen

- **Status:** 🟡 proposed
- **Date:** 2026-10-25
- **Deciders:** Markus Brigl
- **Applies to:** the floor statement in `crates/fleet-core/src/package.rs`, the `package floor` command of `opamp-fleetctl` in `crates/fleet-tools/`, the floors' store in `crates/fleet-server/src/fs/` and `crates/fleet-server/src/postgres/`, the `/api/v1/package-floors` routes in `crates/fleet-server/src/api.rs`, the `/v1/package-floors` route on the Agent plane in `crates/fleet-server/src/transport.rs` and downstream of a Gateway in `crates/fleet-agent/src/gateway/`, the Client's fetch of the floors, the file `package-floors.json` under its `state_dir`, the Supervisor's check of a package offer in `crates/fleet-agent/src/supervisor/agent/packages.rs`, the self-update's check of an offered version in `crates/fleet-agent/src/update/`, and every document that tells an operator how to keep an old signed build off the fleet
- **Supersedes:** [ADR-0043](0043-a-package-version-is-semver-and-never-installed-below-the-high-water-mark.md) clause 13; [ADR-0031](0031-opamp-fleetctl-extracts-a-bootstrap-pkcs12-file-into-pem.md) clause 2; [ADR-0050](0050-the-servers-records-may-live-in-postgresql.md) clause 2

## Context

A package installs only with the operator's signature over its Agent type, version and hash
([ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md) clause 54),
and a Supervisor takes no version below the highest one this Client installed for its type, the
high-water mark
([ADR-0043](0043-a-package-version-is-semver-and-never-installed-below-the-high-water-mark.md)
clause 8). The Client's own update takes no version below the running build (ADR-0043 clause 13,
[ADR-0018](0018-the-client-updates-itself-under-its-own-agent-type.md) clause 4).

A signature never expires. Every build the operator ever signed stays installable wherever its
version lies above what the host has run. `docs/HARDENING.md` names the case: an old build the
operator signed under a version above the current mark, such as a long-ago pre-release of a later
version (`2.0.0-rc.1` while the fleet runs `1.9.4`), is still taken. A Server that is compromised,
or an operator who errs, can offer it to every host. The mark cannot help, because it only knows
what this host installed, and a host that never ran the build has no reason to refuse it.

What closes the gap is a statement only the operator can make: below this version, nothing of this
Agent type is taken any more. It must reach every Client, through Gateways too. A Server that
cannot sign must not be able to lower it.

The specification's quality goal Q-1 asks that no package installs without a valid signature; G-10
and G-11 ask that the Server update an agent's software and the Client's own, verified before it is
applied.

## Decision

We will have the operator sign, per Agent type, a minimum version that the Server stores and serves
to every Client and that each Client keeps as the highest it has seen, and refuse every package
offer of that type below it, the Client's own update included.

1. **A floor is a signed statement of an Agent type and a version.** The statement is
   `opamp-fleet-floor-v1`, the Agent type and the version, one per line, each ended by a newline
   (`fleet_core::package::floor_statement`), signed with the operator's Ed25519 package key. The
   version is SemVer by ADR-0043 clause 2. The prefix differs from the package statement's
   `opamp-fleet-package-v1`, so no package signature verifies as a floor and no floor as a
   package. `opamp-fleetctl package floor --agent-type … --version …` signs it and prints the
   signature in hex, as `package sign` does.

2. **The Server stores one floor per Agent type and serves them to every member.** On the Operator
   plane `PUT /api/v1/package-floors/{agent_type}` with the version and the hex signature stores
   it, `GET /api/v1/package-floors` lists them, and `DELETE /api/v1/package-floors/{agent_type}`
   removes one. A `PUT` whose version SemVer orders below the stored one for that type is `409`; a
   floor is raised, or removed and set anew. Where the Server holds `[packages] verification_key`,
   it checks each floor when it is written and reports it valid or invalid, as it does for a
   package signature: feedback only, since the Client's check decides. The floors are stored as the
   Server stores its other records, in its data directory or in PostgreSQL
   ([ADR-0050](0050-the-servers-records-may-live-in-postgresql.md)), in the data directory as the
   file clause 10 adds to its layout. Every action is audited. On
   the Agent plane `GET /v1/package-floors` answers every admitted member with all floors as a JSON
   object that maps an Agent type to its version and signature, behind the admission of
   `/v1/opamp`.

3. **A Gateway serves the floors it holds to the hosts behind it.** A Gateway fetches the floors
   from its Server when it starts and then once an hour, and answers `GET /v1/package-floors` on
   its downstream listener with what it last fetched, behind the handshake and the revocation
   verdict of its `/v1/opamp`; an enrolling peer is refused them. It checks no signature on their
   behalf; each host checks its own. A host behind a Gateway therefore takes a floor only after the
   Gateway's next fetch, and keeps the old ones while the Gateway's fetches fail.

4. **A Client keeps the highest verified floor of each Agent type.** It fetches the floors from its
   endpoint after its first reply, then once an hour while it is connected, and before it decides a
   package offer when its last fetch is more than a minute old. It verifies each against
   `[packages] verification_key` and keeps, per Agent type, the highest version that verifies, in
   `<state_dir>/package-floors.json`, a JSON object that maps an Agent type to a version and the
   signature it came with, written owner-only through a temporary file and a rename. A floor that
   does not verify, or that orders below the one kept, is ignored and logged. A failed fetch keeps
   the floors held and is logged, and an offer is decided against them. The file lies outside
   `supervisor_dir` and its name holds a `.`, so no purge reaches it (ADR-0043 clause 8). A floor
   is verified when it is kept, not again when the file is read, so a later change of the key does
   not undo it. A file that exists but does not parse, or maps a type to a value that is not
   SemVer, is never read as empty: every package offer, the Client's own included, is refused and
   reported with a reason that names the file, until the operator repairs or deletes it.

5. **No package below its type's floor is taken.** A Supervisor refuses an offer whose version
   SemVer orders below the floor kept for its Agent type, before anything is downloaded, and
   reports it `InstallFailed` with a reason that names the version and the floor. The check stands
   beside the high-water mark and the installed record of ADR-0043 clause 7; the stricter one
   decides. An offer of the package the Supervisor already runs is no install: it is answered as
   installed, below the floor too (clause 6).

6. **What is installed stays.** A Managed Process whose installed version lies below its type's
   floor keeps running, and so does the Client. A Supervisor reports in its Agent's health that
   it runs below the floor, naming both versions, and takes the next offer at or above the floor.
   The Client's own Agent reports no such health: its version is the running build's, and its
   next update at or above the floor replaces it.
   The rollback of ADR-0026 clause 62 puts back the installed predecessor whatever the floor says,
   since it takes no offer, and the health then says so too.

7. **Only the host lowers a floor.** No Server route, no offer, no delivered Supervisor block and no
   key of `supervisor.toml` lowers what `package-floors.json` holds. An operator who wants a
   Client below a floor stops the Client and removes that type's entry from the file, or deletes
   the file, and starts the Client. The Server's `DELETE` only stops serving a floor to the Clients
   that have not kept it.

8. **Restates ADR-0043 clause 13, with the floor added and "not a version" narrowed to "not
   SemVer". The Client's own floor is the higher of its running build and the floor of its Agent
   type.** It refuses an offered version that is not SemVer (ADR-0043 clause 2), one that orders
   below the running build (ADR-0018 clause 4), and one that orders below the floor kept for its
   Agent type. It keeps no high-water mark: the Client's Agent type is not a key in
   `package-high-water.json`. The crash-loop pointer move back to the previous
   version is local, takes no offer, and stays as ADR-0018 decides it.

9. **Restates ADR-0031 clause 2, with `floor` added. One program, two command trees.**
   `opamp-fleetctl package` holds `fetch`, `pack`, `sha256`, `sign`, `keygen` and `public-key`,
   each with the options and output it has today, and `floor` as clause 1 states it.
   `opamp-fleetctl pki` holds `init`, `server-cert`, `bootstrap-ca` and `status` as
   [ADR-0028](0028-one-operator-program-that-makes-the-fleets-certificates.md) states them, and
   `p12-extract` as ADR-0031 states it. `opamp-fleetctl --version` prints the version every binary
   of this project prints ([ADR-0012](0012-versions-resolved-in-the-internal-crate.md)). No other
   program name exists for any of these commands: no alias, no wrapper, no symlink.

10. **Restates ADR-0050 clause 2, with `package-floors.json` added. The layout of `data_dir` is
    fixed, and no other is read.** With `store = "fs"`, under `data_dir`:

    | Path | One file per | Domain |
    |---|---|---|
    | `configurations/<name>.json` | Configuration, with its retained revisions | Configurations ([ADR-0034](0034-a-deployment-carries-configurations-and-is-the-one-thing-rolled-out.md)) |
    | `deployments/<name>.json` | Deployment | Deployments |
    | `packages/<agent_type>@<version>.json` | Package: identity and entries, no bytes | Packages ([ADR-0033](0033-a-package-entry-carries-its-signature-and-every-artifact-is-uploaded.md)) |
    | `blobs/<sha256>` | artifact, with the filesystem blob store | Blobs ([ADR-0049](0049-artifact-bytes-may-live-in-an-s3-compatible-object-store.md)) |
    | `agents/<instance_uid>.json` | Agent: report, assignment, labels | Fleet ([ADR-0035](0035-a-host-is-what-a-client-certificate-names-and-its-agents-inherit-from-it.md)) |
    | `hosts/<host>.json` | Host | Hosts |
    | `certificates/<issuer>-<serial>.json` | certificate the Server signed | Certificate register |
    | `revocations.json` | the revocation list | Revocations |
    | `bootstrap/<issuer>-<serial>.json`, `.p12` | bootstrap certificate the Server issued | Bootstrap |
    | `enrolment.json` | the window and its pending requests | Enrolment (ADR-0050 clause 9) |
    | `package-floors.json` | the floor of every Agent type | Package floors (clause 2) |
    | `audit/audit-<first seq>.jsonl` | audit file | Audit |

    With `store = "postgres"`, `data_dir` holds `audit/` and, with the filesystem blob store,
    `blobs/`, and nothing else. Beside a bucket, a `blobs/` an earlier filesystem blob store left is
    accepted too, as the place
    [ADR-0049](0049-artifact-bytes-may-live-in-an-s3-compatible-object-store.md) clause 7 copies
    from; the Server does not create it there.

    `<issuer>` is the first 16 hex characters of the SHA-256 of the issuer's DER name, and `<serial>`
    is the serial in the register's lowercase hex form. A file or directory the layout does not
    name, a file that does not parse, or a file whose name disagrees with its content fails startup,
    naming the path. A path of the filesystem layout found under a `data_dir` used with
    `"postgres"` fails startup naming it and `server migrate-to-postgres`. The one exception is a
    temporary file the Server's own write left behind — a regular file whose name ends in `.tmp`,
    whose rename never happened: it is removed at startup, since the record beside it is the one
    before the write.

**Out of scope:** a list of withdrawn builds by hash; an expiry or a signing time in the package
statement; rotating the package key; a floor per Platform or per Deployment; the floors in the
bundled UI, beyond what the REST API serves; a Server that withholds a floor, which delays it until
the Client reaches one that serves it.

## Alternatives considered

- **A signed list of withdrawn builds by hash.** It is exact, but the operator must know and name
  every bad build, and the list only grows. A floor names one version and covers every build below
  it, known or not. Not taken; it stays possible beside a floor.
- **A signing time in every package statement and a signed cut-off date.** It would also catch
  builds whose version nobody remembers, but it changes the statement of ADR-0026 clause 54, and
  every signature the fleet holds would have to be made again. Rejected.
- **Rotating the package key.** It removes every old signature at once, and with it every good one.
  It is a fleet-wide operation with no counterpart in the Client today. Rejected as the way to
  withdraw one build.
- **Carrying the floor in the package offer**, as an extra entry of `packages_available` under a
  reserved name. It needs no route, but it gives a protocol field a second meaning that another
  OpAMP implementation would read as a package. Rejected.
- **A `CustomMessage`.** It is `[Development]` in the Baseline and not implemented here
  ([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
  rejected it for the revocation list for the same reason). Rejected.
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

- The Update Framework, *Specification*, the client workflow and the *rollback* and
  *freeze* attacks it names: a repository that serves validly signed but outdated metadata or
  packages, which TUF answers with version numbers that only rise and a signed timestamp.
- Android Verified Boot, *rollback protection*: a minimum version kept on the device that only
  rises, below which no signed image boots.
- Mozilla's add-on and Chrome's extension blocklists, as the alternative of withdrawing builds by
  identity.
- Recalled, not re-read for this record: the TUF, Android Verified Boot and blocklist references
  above, cited for their shape (a version that only rises, withdrawal by identity), not for detail.
- This repository: [ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)
  clauses 54 and 62, [ADR-0043](0043-a-package-version-is-semver-and-never-installed-below-the-high-water-mark.md)
  clauses 2, 7, 8, 11, 12 and 13, [ADR-0018](0018-the-client-updates-itself-under-its-own-agent-type.md)
  clause 4, [ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
  (the Gateways' revocation route on the Agent plane), `docs/HARDENING.md`.

## Consequences

- Positive: an operator can keep any build below a version off the whole fleet with one signature,
  without knowing which builds exist. A compromised Server can neither forge nor lower a floor. The
  residual that `docs/HARDENING.md` names for old signed builds closes, for the Client's own
  updates too.
- Negative / trade-offs: one more signed statement, store, route and state file. A Server or
  Gateway can withhold a floor, so it takes effect on a host only once a Server or Gateway that
  serves it has been reached, and behind a Gateway only after the Gateway's next fetch. A
  floor set too high cannot be taken back from the Server. Each host must be lowered by hand, or
  the operator publishes the old content as a new, greater version (ADR-0043 clause 12). Every
  Client asks for the floors once an hour, and before an offer at most once a minute. The floors'
  store is a schema change for PostgreSQL. With the filesystem store it is a file the layout of an
  earlier Server does not name, so that Server refuses to start on the data directory until the
  file is moved away.
- Follow-ups: the floors in the bundled UI; a list of withdrawn builds beside the floor; rotating
  the package key.

## Enforcement

Tests that will carry `Verifies: ADR-0061`:

- In `crates/fleet-core/`: the floor statement is the three lines of clause 1.
- In `crates/fleet-tools/`: the `package` tree names `floor` (clause 9).
- In `crates/fleet-server/`: `PUT`, `GET` and `DELETE` of `/api/v1/package-floors` in both stores,
  a `PUT` below the stored floor answered `409`, the check against `[packages] verification_key`
  reported but never refused, a package signature reported as no floor, every change audited,
  the floors' file part of the `data_dir` layout (clause 10), and `GET /v1/package-floors`
  admitted as `/v1/opamp` is, refused to a revoked certificate and to an enrolling host.
- In `crates/fleet-agent/tests/`: a host behind a Gateway receives the floors the Gateway last
  fetched, behind the downstream admission, and an enrolling peer is refused them.
- In `crates/fleet-agent/src/`: a floor that does not verify, a package signature among them, or
  that is lower than the kept one, is ignored; a failed fetch keeps the kept floors; an unreadable
  `package-floors.json` refuses every offer, the Client's own included; a Supervisor refuses an
  offer below its type's floor before downloading and reports `InstallFailed`, and answers an offer
  of what it runs as installed; a Managed Process below the floor keeps running and its health says
  so; a rollback to a predecessor below the floor still happens; the self-update refuses a version
  below its type's floor.
