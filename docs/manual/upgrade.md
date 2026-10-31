# Upgrading from 0.5

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[A host's life](lifecycle.md)

This page takes a fleet whose Server and Clients run a 0.5 release to the release this manual
describes, in one pass. The Server goes first, then the Clients, which update themselves from it.
No release in between is needed: a 0.5 Client and this Server speak the same protocol, over the
same TLS 1.3 with the same client certificates, and agree on the package offer, the download path,
the package signature and the renewal proof. A 0.5.2 Client takes this release's `supervisor`
Package as it took any other.

What changed is what each end accepts in its own files and records. Most of the work is cleaning
those up while the hosts still run 0.5, before a binary arrives that refuses what they hold.
[`CHANGELOG.md`](../../CHANGELOG.md) has every change with its own *What to do*; this page puts the
ones a 0.5 fleet meets in order, and links to the sections of this manual that already describe a
step.

[What to try on a test host first](#what-to-try-on-a-test-host-first) lists what to prove before
the fleet takes these steps.

- [At a glance](#at-a-glance)
- [Before you start](#before-you-start)
- [1. Clean up the Clients' files](#1-clean-up-the-clients-files)
- [2. Upgrade the Server](#2-upgrade-the-server)
- [3. Make the Server offer again](#3-make-the-server-offer-again)
- [4. Roll the new Client out](#4-roll-the-new-client-out)
- [5. After the update](#5-after-the-update)
- [What a 0.5 host still meets](#what-a-05-host-still-meets)
- [Scripts against the REST API](#scripts-against-the-rest-api)
- [Going back](#going-back)
- [What to try on a test host first](#what-to-try-on-a-test-host-first)

## At a glance

| Step | Where | Why it comes here |
|---|---|---|
| 1. Clean up the Clients' files | every host, and the Server's Supervisor sets | The new Client refuses at startup what 0.5 accepted. Every change is one a 0.5 Client accepts too; one waits for step 2. |
| 2. Upgrade the Server | the Server host | It reads only one data directory, and `server migrate` moves the old state there. 0.5 Clients keep connecting. |
| 3. Make the Server offer again | **Configurations**, **Packages** and **Deployments** tabs | The migration drops every Configuration's Selector, and nothing is offered until a Deployment holds it. |
| 4. Roll the new Client out | **Packages** and **Deployments** tabs | A `supervisor` Package, a canary channel first. |
| 5. After the update | hosts, as you choose | The systemd unit, the account, the certificate's subject values, a floor. |

## Before you start

- **Know which 0.5 you run.** These steps start from 0.5.2 or later. A fleet on 0.5.0 or 0.5.1
  still carries the fleet credential: follow
  [Upgrading to admission by certificate alone](server.md#upgrading-to-admission-by-certificate-alone)
  as part of step 2, and read the 0.5.1 entries of the changelog, which ask you to mark each
  Gateway's host before that Gateway is upgraded.
- **Keep the trust as it is.** The server CA, the Server's certificate, the client CA's
  certificate and key in [`[client_ca]`](server.md#client_ca), the bootstrap CA and the package
  signing key all stay. Every Client is admitted by a certificate that verifies against the client
  CA's key, trusts the Server through the CA in its `[tls] ca_file`, and takes only packages signed
  with the key its `[packages] verification_key` names. A new one of any of these shuts out every
  host that has not been given it, whatever release it runs. Two checks remain: a bootstrap CA of
  your own PKI must write an organization and an organizational unit, the country as a
  `PrintableString` and every other value as a `UTF8String` or a `PrintableString`, each at most
  once; and a Gateway reads the revocation list within a bound that holds the list of up to
  eight client CA certificates, so keep `client_ca_file` to eight or fewer.
- **Get this release**, the one this manual comes with: an earlier release lacks some of what
  this page names, such as the floor. The Server runs from its container image
  ([In a container](server.md#in-a-container)) or from a build. For the Clients, download
  `supervisor_<version>_<os>_<arch>.tar.gz` of every platform your fleet runs, and
  `opamp-fleetctl` for the machine that holds the signing key. Verify each download against the
  release's attestation and `SHA256SUMS`, as [Getting it](tools.md#getting-it) shows.
- **Pick test hosts**, one per platform and one behind each kind of Gateway you run, and put them
  in a channel of their own ([The canary shape](server.md#the-canary-shape)).

## 1. Clean up the Clients' files

A Client's configuration lives in two places: each host's `supervisor.toml`, and the
Configurations typed `supervisor` whose `[[supervisor]]` blocks a Client writes into that file
([The Server can manage the set](client.md#the-server-can-manage-the-set)). Change the
Configurations on the 0.5 Server and roll them out, so the hosts' files carry the change. Make the
rest of the edits on the hosts, by hand or by configuration management. A Supervisor set carries no
`[packages]` section, so the first item below is a host-side edit everywhere.

Do this before step 4. A self-updated Client that cannot read its file counts that as a failed
start: after three it is rolled back to 0.5, which reports the update `InstallFailed`. The host
keeps running, on the old release. A host upgraded by its package manager has no such rollback,
and its service stays down.

- **Delete `[packages] allowed_sources`**, even `allowed_sources = []`. A Client downloads from
  its own Server and from nowhere else, and a file that names the key fails its start with
  `[packages] allowed_sources is removed`. A 0.5 Client without the key downloads from its Server
  alone too, so a Package whose entries only reference an artifact elsewhere, or that
  `advertised_url` offers from a mirror, stops installing on it. Delete the key after step 2,
  which ends both, and before step 4.
- **Replace each block of a kind this build no longer carries.** Its `type` is unknown to this
  Client, so the file fails its start and an offered set naming it is refused. Run the same
  program under a [`type = "command"`](client.md#type--command) block: its program, its arguments
  and its environment, and [`program_path`](client.md#agents-that-are-more-than-one-file) where
  its package is a whole tree. Set `service_name` to the Agent type the old block presented, so the
  Configurations and Packages made for that type still reach it. A 0.5 Client runs the `command`
  block the same way.
- **Rename a Supervisor whose directory would collide with the Client's own.** Where
  `supervisor_dir` is `state_dir`, a Supervisor may not be named `instance-uid`, `config`,
  `packages`, `logs`, `gateway-packages` or `supervisors`, and on every host none may have a
  directory that would be `state_dir` or hold it. The default `supervisor_dir`,
  `<state_dir>/supervisors`, is not affected. A renamed Supervisor registers as a new Agent: set
  its labels again.
- **Give a state directory that is a link its real path.** The new Client refuses a `state_dir`
  that is itself a symbolic link, on Windows a junction, before it counts the start, so no
  rollback follows: the service restarts it and it fails each time. With the 0.5 Client stopped,
  move the directory to its real path, register the service with
  `service install --state-dir <real path>`, and start it again.
- **Name a Collector's type where it differs from its program's file name.** An Agent's type is
  always its Supervisor's: the block's `service_name`, else the program's file name. The
  `dist.name` a Collector reports no longer decides it. If your Configurations, Deployments or
  Packages are made for the `dist.name`, set `service_name = "<dist.name>"` in the block. A 0.5
  Client takes the key too. See [Keys every block accepts](client.md#keys-every-block-accepts).
- **Write a variable a Supervisor set may no longer bring into the host's file.** An offered set
  is refused whole when a block brings `PATHEXT`, `COMSPEC`, `NODE_PATH`, `PYTHONHOME`, `PERLLIB`,
  `PERL5DB` or `RUBYLIB`, or a new value that starts with a path under the Supervisors' root.
  Set such a variable in the host's `supervisor.toml` by hand.
- **On Windows, restrict a `supervisor_dir` outside the data root, the layout root and the state
  directory yourself**, to LocalSystem, the Administrators and the service account. An install or
  an MSI upgrade leaves an existing one as it is.
- **Move a Selector off a Collector's own attributes.** A Managed Process now states only its own
  `service.*` attributes; its other attributes, its resource attributes and its own `host.name`
  no longer reach the Server. A Deployment whose Selector matched one of them stops reaching the
  updated host: put that tag into a Server label or into
  [`[attributes]`](client.md#attributes) instead. A reported attribute whose key starts `fleet.` is
  left out of what Selectors match, too.

## 2. Upgrade the Server

1. **Give every Configuration an Agent type, on the 0.5 Server.** This Server refuses a stored
   Configuration without one, and the migration does not carry it. Save each one again with its
   `service_name`. Split content meant for several Agent types, such as a certificate bundle,
   into one Configuration per type.
2. **Upload what a Package entry only references.** An entry is an uploaded artifact and nothing
   else. Upload each referenced artifact as a file to its entry, or do it after the migration, which
   drops such an entry and names it. Either way the entries are signed after the migration
   (section 3). `opamp-fleetctl package fetch` does it for
   the agents it knows.
3. **Edit `server.toml`.** Remove `advertised_url`: every package is offered as a path on this
   Server. Make `[connection_offer] endpoint` a `wss://` or `https://` URL, or remove it: an offer
   never moves a Client to plaintext, and the Server refuses to start on one. The Server refuses
   either key at startup, naming it. A TLS-terminating proxy in front of the operator plane keeps
   the `Host` the browser sent, or a body-less `POST` from the UI is refused `403`.
4. **Stop the Server and migrate**, as
   [Upgrading to one data directory](server.md#upgrading-to-one-data-directory) says, step by
   step: `server migrate` copies `config_dir` and `packages_dir` into a new `data_dir`, leaves the
   old ones as your backup, and prints what it carried, dropped and listed. Keep that output;
   section 3 below works through it. Under systemd's `ProtectSystem=strict`, a `data_dir` outside
   the unit's `StateDirectory` needs a `ReadWritePaths=` line.
5. **Start this Server.** A `--config` that names no file now stops it, and so does a
   `FLEET_SERVER__…` variable that names no key
   ([Configuration from the environment](server.md#configuration-from-the-environment)). It warns
   about each Package stored under a version that is not SemVer. Every Client that holds a valid
   certificate connects as before, and each Agent keeps running what it runs.

`[packages] verification_key` in `server.toml`, the public key the Clients hold, is optional. With
it the **Packages** tab shows each entry's signature as `valid` or `invalid` before a rollout,
which is worth having for the next step ([`[packages]`](server.md#packages)).

The Server of every 0.5 release before 0.5.6 could be made to renew one host's certificate over
another host's key. After the upgrade, look through `GET /api/v1/certificates` for a renewal you
cannot account for: each entry names its `predecessor`, and the audit record holds an
`issuance.signed` line for each renewal, with the `instance_uid` of the message that carried it.
Revoke what you cannot explain
([Revocation](server.md#revocation-withdrawing-a-certificate)).

## 3. Make the Server offer again

A 0.5 Server released Configurations by their own Selector. This one releases them only through a
Deployment, and the migration kept none of those releases. Nothing changes on a host meanwhile.

1. **Put each Configuration the migration listed into a Deployment** whose Selector reaches the
   Agents it aimed at. An Agent belongs to at most one Deployment, so a Configuration meant for
   the whole fleet goes into every channel.
2. **Sign each entry the migration named unsigned**, and upload each artifact it dropped
   ([Create the Package, upload its entries, sign them](server.md#create-the-package-upload-its-entries-sign-them)).
3. **Republish each Package whose version is not SemVer** under one that is: `1.19` becomes
   `1.19.0`. Sign it again and put it into its Deployments. Such a Package is offered to nobody,
   and an updated Client refuses it
   ([A Supervisor never goes back](client.md#a-supervisor-never-goes-back)).
4. **Roll each Deployment out**, and check that its Agents report their Configurations
   **Applied**.

## 4. Roll the new Client out

The path is the one every self-update takes
([Updating the Client itself](client.md#updating-the-client-itself), and the shorter
[The Client updates itself the same way](lifecycle.md#the-client-updates-itself-the-same-way)).
What a 0.5 fleet has to get right:

1. **Make the Package.** Its Agent type is `supervisor`, and its version is exactly the release
   the archives carry: `<version>` for the archives `supervisor_<version>_<os>_<arch>.tar.gz`. A
   build of your own reports a version such as `<version>-dev+1a2b3c4`; its Package is
   `<version>-dev`, because the staged binary has to report the version offered, apart from the
   commit. A `-dev` version orders below the release of the same number, so a host that took the
   release refuses such a build afterwards. Upload each platform's
   archive as it was downloaded, one entry per platform. Sign the entries with the key whose
   public half the Clients hold as `[packages] verification_key`
   ([Signing a whole Package at once](tools.md#signing-a-whole-package-at-once)).
2. **Put it into the canary channel first.** A label on a Client's own Agent holds for every Agent
   of that Client, so labelling the test hosts' Client Agents with `channel = canary` moves each
   whole host. Give the canary Deployment the Configurations and Packages the host's usual channel
   holds, add the `supervisor` Package, and roll it out. A channel holds one Package per Agent
   type: one that already holds a `supervisor` Package refuses a second with `409`, and
   `?replace=true` swaps it
   ([Put it in a channel, roll it out](server.md#put-it-in-a-channel-roll-it-out)).
3. **Watch the Client's own Agent** on the **Agents** tab: `Downloading`, `Installing`, then
   `Installed` and the new `service.version`. On the host, the 0.5 Client stages the new binary
   beside the running one, runs its `self-check`, and exits so the service manager starts it with
   the same command line. The new version commits itself once it reaches the Server.
4. **Read an `InstallFailed`.** A failure before the switch names its reason in the report: the
   signature, the version, the platform. After a rollback the host runs 0.5 again, and the reason
   is in the new version's lines of the Client's log
   ([Where the service's logs are](client.md#where-the-services-logs-are)). The rows of
   [the Client's troubleshooting](client.md#troubleshooting) name the messages a file meets. Fix the
   file before the next attempt.
5. **Move the release** once the canary hosts run it: give each channel the same Package and roll
   it out ([The canary shape](server.md#the-canary-shape)). A Gateway is a Client and updates the
   same way; put one and a host behind it among the test hosts.

**What the update keeps.** A self-update changes the binary and nothing else: the host keeps its
service registration, its certificate and enrolment, its state directory, and the account the
service runs under. That is root for a host installed from a 0.5 `.deb` or `.rpm`, and on Windows
`LocalSystem` or the account `--run-as` named. The new defaults for those, the system account
`opamp-fleet` and Windows' virtual account `NT SERVICE\opamp-fleet`, apply to a first install only.

**A host that cannot update itself** needs its package manager or a hand: one whose Client is not
installed as a service, or holds `self_update = false`. A `.deb` or `.rpm` upgrade re-registers
the service and keeps the account its unit names
([Installing from a native package](client.md#installing-from-a-native-package)).
An MSI upgrade keeps the account the SCM holds, and stops the Client and its Managed Processes for
as long as it runs. Neither rolls back, so clean the file up first.

## 5. After the update

None of these is needed for the fleet to run.

- **The systemd unit.** A host that updated itself still runs from the unit 0.5 wrote. The next
  `service install` writes this release's unit, with its sandboxing
  ([The systemd unit, and what a drop-in adds](client.md#the-systemd-unit-and-what-a-drop-in-adds)).
- **The account.** Move a host off root or `LocalSystem` with `service install --run-as …`, after
  checking that its agents work without the privilege
  ([Running it under its own account](client.md#running-it-under-its-own-account)). A process
  account of its own for the Managed Processes is a further step
  ([Managed Processes under a process account](client.md#managed-processes-under-a-process-account)).
- **The `fleet.host.*` values.** A certificate the Server issued before 0.5.5 carries no subject
  values, and its renewals carry none either. Its host's Agents match no `fleet.host.*` Selector
  until the host enrols again with a bootstrap certificate that names them
  ([Enrol the host](lifecycle.md#3-enrol-the-host)).
- **A floor.** Once every host runs this release, a signed floor for `supervisor` at its version
  keeps every older build off the fleet
  ([Keeping old builds off the fleet](tools.md#keeping-old-builds-off-the-fleet)).
- **A Gateway's rate limit.** A Gateway now limits each host behind it as the Server limits a
  host, by default 10 messages a second with a burst of 300. Where the hosts behind a Gateway
  send more, raise `[gateway.rate_limit]` once that Gateway runs this release; a 0.5 Gateway
  refuses the section and does not start.
- **Enrolment through a Gateway.** A Gateway with `[gateway.tls] bootstrap_ca_file` relays the
  enrolment of the hosts behind it
  ([Enrolment through a Gateway](client.md#enrolment-through-a-gateway)).
- **The old directories.** Delete `config_dir` and `packages_dir` once you are satisfied.

## What a 0.5 host still meets

A host keeps running 0.5 until its update arrives, and some do for longer. Against this Server:

- **It connects, applies and installs as before,** and reports what it runs.
- **A new host does not enrol with a 0.5 Client as it did.** The Server hands out a bootstrap
  certificate only as a password-protected `bootstrap.p12`, which a 0.5.2 Client cannot read; take
  the pair out with `opamp-fleetctl pki p12-extract` ([`pki`](tools.md#pki)). Better, install new
  hosts from this release. A new bootstrap certificate's password is 8 to 128 characters, and one
  bootstrap certificate holds at most 256 places in the enrolment queue
  ([`[enrolment]`](server.md#enrolment)).
- **A floor does not protect it.** A 0.5 Client does not read the floors; only an updated one does.
- **Behind a Gateway, a host on a certificate you provisioned** — one the Server did not issue —
  gets no new certificate through the Gateway. Give it its next one yourself, or let it enrol
  directly with the Server. A host on a certificate the Server issued renews through a Gateway as
  before.
- **Once updated, it does not go back.** The Client refuses an update to an older version, and a
  Supervisor a package below the highest version this Client installed for its type.

## Scripts against the REST API

What a script written for 0.5.2 meets; each item is a changelog entry with its *What to do*:

- A Configuration is `{"body", "service_name", "role"}`: `service_name` is required, a `selector`
  is answered `400`, and `POST /api/v1/configurations/{name}/rollout` answers `404`. A Deployment
  holds Configurations (`PUT /api/v1/deployments/{name}/configurations/{configuration}`), and its
  rollout releases them.
- A signature goes on the entry: `PUT …/entries/{os}/{arch}/signature`. The Deployment's signature
  routes and the entry's `…/source` route answer `404`.
- A Package version is SemVer, or the `PUT` is answered `400`.
- `POST /api/v1/bootstrap-certificates` needs a `password` of 8 to 128 characters and a `subject`
  naming `organization` and `organizational_unit`. Its answer carries no `private_key_pem`:
  download `GET /api/v1/bootstrap-certificates/{serial}/p12`.
- `GET /api/v1/enrolments` lists the bootstrap certificate's values as the object
  `bootstrap_subject`, and no `subject`.
- A label key starting `fleet.` is answered `400`.
- Deleting a Configuration, a Package or a Deployment that an Agent's assignment pins answers
  `409`.
- The per-Agent rollout takes an empty body or `{"deployment": "<name>"}`; naming a Configuration
  or a Package is refused.

## Going back

- **The Server.** The old `config_dir` and `packages_dir` are the backup, as they were when the
  migration ran: a 0.5 Server started on them has lost what changed since. A 0.5 Server reads no
  `data_dir`, and records moved into PostgreSQL do not come back.
- **A Client.** No self-update goes back. A host given a 0.5 binary by hand comes up under it and
  is updated forward again at the next rollout, unless it holds `self_update = false`. A host with
  a process account drops it first
  ([Before going back to an older release](client.md#before-going-back-to-an-older-release)).
  Try a 0.5 Client on a state directory this release wrote on a test host before you rely on it.

## What to try on a test host first

Before the fleet takes these steps, prove on a test host:

- **The whole self-update, per platform**: Linux under systemd, on amd64 and arm64, macOS under
  launchd, Windows under the SCM. A 0.5 Client installed as a service takes the `supervisor`
  Package, runs the self-check, is restarted on the new binary and reports `Installed`.
- **A rollback**: a host whose file the new Client refuses fails three starts, comes back on 0.5
  and reports `InstallFailed`. Check whether it takes the same offer again once the file is fixed,
  or needs another rollout.
- **The new binary on a real 0.5 state directory**: under the account the host runs as, with the
  owners and modes 0.5 left. Its issued certificate, its Agents' `instance-uid` files and their
  installed packages carry over, and its next renewal succeeds.
- **`server migrate` on a copy of your real directories**: the register, the revocation list, the
  Hosts with the Agents bound to them, the bootstrap certificates and the audit, checked with
  `server audit-verify <data_dir>/audit`. Then a 0.5 Client connects to the migrated Server,
  renews, and a revoked certificate stays refused.
- **A Gateway and a host behind it**, both on 0.5, updated one after the other in the order you
  will use.
- **Each block you replaced with a `command` block**: the Agent keeps its type, takes its
  Configuration and its next Package.
