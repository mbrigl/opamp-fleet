# Artifact: Telegraf

[← User Manual](../manual/README.md) · [The Client](../manual/client.md) ·
[Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how Telegraf is packed, and whoever changes what the `telegraf`
kind knows. It is the one place both sides state the same facts, so a release that moves something
can be answered on both at once ([ADR-0017](../adr/0017-supervisor-mode-and-its-kinds.md) clause 20,
[ADR-draft-telegraf](../adr/draft-telegraf.md)).

It is a thin artifact document, because the artifact is installed exactly as InfluxData
published it: there is no repack to keep in step, and no tree whose internal layout
anybody could get wrong.

| | |
|---|---|
| Packed by | `telegraf_plans` in `crates/fleet-tools/src/fetch.rs` |
| Run by | `crates/fleet-agent/src/supervisor/telegraf.rs` |
| Agent type | `telegraf` |

## 1. Source

Versions come from the GitHub tags of **`influxdata/telegraf`**, spelled `v<major>.<minor>.<patch>`
— three numeric parts and nothing else, which is what keeps release candidates (`v1.21.0-rc1`) out
of the list an operator picks from.

A Package version is SemVer
([ADR-0028](../adr/0028-package-versions-high-water-mark-and-floor.md) clause 6), and such a tag
already is one behind its `v`: release `v1.39.3` is fetched under that tag and uploaded as Package
`telegraf` `1.39.3`. The mapping is the identity, and no Telegraf release needs the `.0`
completion or the leading-zero rule.

The **binaries do not live there**. They are served from
`https://dl.influxdata.com/telegraf/releases/`, a CDN with no directory listing, so the platform
list cannot be read from anywhere and is this tool's own (`TELEGRAF_PLATFORMS`).

## 2. Assets per platform

`telegraf-<version>_<os>_<arch>.tar.gz`, and `.zip` for Windows. Upstream spells 32-bit `i386`
where this fleet says `386` ([ADR-0027](../adr/0027-packages-signatures-and-delivery.md)); the
mapping is the third and fourth column of `TELEGRAF_PLATFORMS`:

| This fleet | Upstream |
|---|---|
| `linux/amd64`, `linux/arm64`, `linux/386` | `linux/amd64`, `linux/arm64`, `linux/i386` |
| `darwin/amd64`, `darwin/arm64` | the same |
| `windows/amd64`, `windows/arm64` | the same, as `.zip` |

A URL that has gone away is found by the download failing, not by a check — which is the price of a
list nobody publishes.

## 3. Integrity

Two origins state each artifact's SHA-256, and the artifact comes from only one of them
([ADR-draft-telegraf](../adr/draft-telegraf.md) clause 3):

- **The release notes on GitHub** (`https://github.com/influxdata/telegraf/releases/tag/v<version>`,
  read through the API as the release's `body`) carry a table of
  `<arch> | <os> | <asset as a link> | <sha256>` rows, the name and the digest in backticks
  before 1.28. The artifact's row is found by the asset's exact name in its link. **This digest is
  required:** an artifact the notes name nowhere is refused when it is fetched, because then only
  the host that serves it would vouch for it.
- **`<asset-url>.DIGESTS`**, beside each artifact on the CDN: `sha256sum`-style `<hash>  <name>`
  lines, the artifact's looked up **by name**. Where it exists it must agree with the notes, or
  the fetch fails naming both digests. **From 1.39.0 on** it is published beside every artifact,
  so a `404` there fails the fetch. **Before 1.39.0** it exists only for a few artifacts of a few
  releases from 1.36.4 on; a `404` is its absence and the notes decide alone.

**`<asset-url>.asc`**, beside each artifact on the CDN, is InfluxData's own OpenPGP signature of
it. It is verified with `gpgv` against InfluxData's package-signing key, which the tool carries in
`crates/fleet-tools/trust/influxdata-archive.gpg` (from
`https://repos.influxdata.com/influxdata-archive.key`). The signature must be valid and made by the
primary key `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927` or one of its signing subkeys: the
2023 subkey `…7DF8 B07E`, which signed up to 1.37.0 and expired on 2026-01-17, or the 2025 subkey
`…0585 BD3B`, which signs from 1.37.1 and expires on 2029-01-17. A subkey that has expired since it
signed still counts. A bad signature, or one by any other key, fails the fetch.

- **From 1.34.4 on** InfluxData publishes an `.asc` beside every artifact, so a `404` there fails
  the fetch.
- **Before 1.34.4** a few releases carry one for the linux/amd64 archive and the rest none. A
  `404` is the signature's absence and the digests decide; an `.asc` that is published must still
  verify.

The check runs `gpgv`, which has to be installed on the host that fetches: Debian and Ubuntu have
it with `apt`, macOS and Windows get it with GnuPG. Without it a fetch that has to check a signature
fails, naming `gpgv`. `gpgv` does not check revocation.

## 4. Treatment

**As published.** The bytes InfluxData serves are the bytes this fleet distributes; nothing is
unpacked, gathered or rewritten.

## 5. Form in the delivered tree

A **single-file package** ([ADR-0027](../adr/0027-packages-signatures-and-delivery.md)). The
archive wraps everything in a version-named directory. On Windows the program is `telegraf.exe`
directly inside it, beside `telegraf.conf`. On Unix it is `usr/bin/telegraf`, and it is not the
only entry of that name: the tarball also holds the logrotate rule `etc/logrotate.d/telegraf` and
the directories `etc/telegraf/`, `usr/lib/telegraf/` and `var/log/telegraf/`.

So the file name does not say which member is the program, and the kind names the member by its
place below the version-named directory: `usr/bin/telegraf` on Unix, `telegraf.exe` on Windows
(`MEMBER` in the kind, [ADR-draft-telegraf](../adr/draft-telegraf.md) clause 3). The Client
takes the one file whose trailing path components are that place, wherever the archive keeps it in
its order, and refuses an archive in which it matches more than one
([ADR-draft-packages-signatures-and-delivery](../adr/draft-packages-signatures-and-delivery.md)
clause 55). Only the program is unpacked, so the kind still has no `program_path`.

The installed program therefore lands in this Supervisor's own `program/` directory, which is also
where the process starts ([ADR-0017](../adr/0017-supervisor-mode-and-its-kinds.md)).

## 6. What the Client derives

| | Unix | Windows |
|---|---|---|
| program | `telegraf` | `telegraf.exe` |
| member in the archive | `usr/bin/telegraf` | `telegraf.exe` |
| `program_path` | — (single file) | — |
| `service_name` | `telegraf` | `telegraf` |
| arguments | `--config <config_dir>/telegraf-conf` | the same |
| version | `--version`, read as strict SemVer | the same |
| preflight | `--version` against the staged program | the same |
| reload | `SIGHUP` | restart |
| `endpoint_port` | refused — Telegraf speaks no OpAMP to us | refused |

The **reload** is why this is a kind at all. `SIGHUP` is Telegraf's documented way of re-reading its
configuration — the same signal `systemctl reload` sends — and as a block key it was refused on
Windows at parse time, so one Supervisor set could not serve a mixed fleet. Here the signal is used
where signals exist and the Runner's restart stands in where they do not, with nothing said in the
block.

What the kind's Supervisor reports of Telegraf is the version alone: the probe yields
`service.version`, which is the one key a Managed Process contributes here
([ADR-0016](../adr/0016-what-an-agent-reports-about-itself.md)). Telegraf speaks no OpAMP, so
it states nothing else; the host, the platform and the type are the Supervisor's.

The **version arguments serve twice**: as the probe that gives the Agent its `service.version`, and
as the preflight run against a *staged* program before the running one is stopped
([ADR-0017](../adr/0017-supervisor-mode-and-its-kinds.md) clause 54). What makes them a version
probe is what makes them a safe check — cheap, and touching no state.

## 7. Configurations

| Name | Agent type | Body |
|---|---|---|
| `telegraf-conf` | `telegraf` | `config/examples/telegraf-conf.toml` |

`opamp-fleetctl package fetch` uploads it beside the package when the Server has none of that name
yet, and puts it into no Deployment: it reaches a host once an operator adds it to the Deployment
that should carry it and rolls that out. The Configuration's name is also the file name its entry
gets in the Supervisor's `config/` directory, which is what `--config` points at — so **the name
is not decoration**: renaming it on the Server without renaming `CONFIG_ENTRY` here leaves
Telegraf pointed at a file that does not exist.

## 8. The account it runs under

Telegraf runs as a Managed Process of the Client, so it runs under the Client's account and inside
the Client's systemd unit ([ADR-0037](../adr/0037-the-accounts-the-service-runs-under.md)). A Linux
package installs the Client under an account of its own, not root.

- **Inputs that read only what any account may read** — `internal`, `cpu`, `mem`, `disk`, `net`,
  `system`, the network listeners above port 1024 — work as they are.
- **Inputs that need root** — `smart`, `ipmi_sensor`, and others that read devices or run a
  privileged helper — fail under the account. The unit deliberately sets no `NoNewPrivileges=`,
  so `sudo` keeps working for every Managed Process: with `use_sudo = true` in the input and a
  `sudoers` rule that lets the Client's account run exactly that command without a password, the
  input works. Without such a rule it fails, and Telegraf logs the error.
- **Root itself is the operator's choice, per host:** `sudo opamp-fleet service install --run-as
  root` registers the Client, and with it Telegraf, as root, and every upgrade keeps it so. Under
  root `use_sudo` is not needed. Nothing in the fleet makes that choice for a host, and the kind
  has no key for it.
- **Where a process account is registered**
  ([ADR-0037](../adr/0037-the-accounts-the-service-runs-under.md) clause 18), Telegraf runs under
  that account instead, started by the second service, and that account is never root, whatever
  the Client runs as. Its unit sets no `NoNewPrivileges=` either, so the
  `sudoers` rule for `use_sudo` names the process account; an input that needs root and has no
  such rule fails there even on a host whose Client runs as root.

What the unit carries under each account, and what a drop-in adds, is in
[the Client manual](../manual/client.md#the-systemd-unit-and-what-a-drop-in-adds). On Windows a
first registration runs the service, and with it Telegraf, as its own virtual account
`NT SERVICE\<PRODUCT_NAME>`; a re-registration keeps the account the service already had,
`LocalSystem` included.

## 9. What can change, and what goes red

No test here reaches upstream, so none can see a release move something. What the tests pin is
what this repository writes down, on each side, against the facts this document states; when
upstream moves one of them, the column *caught by* says what an operator meets first.

| An upstream that… | breaks | caught by |
|---|---|---|
| renames its assets or changes the `_<os>_<arch>` spelling | the download | the download failing with the `404`; `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` (packing side) pins the spelling of [section 2](#2-assets-per-platform) |
| moves or renames `.DIGESTS` | every fetch from 1.39.0 on: refused, naming the `404`; before it the `404` reads as absent, and the notes decide alone | the refusal; `digests_that_are_not_published_leave_the_release_notes_to_decide` pins the version from which it is required, and the `ChecksumSource` assertion of the packing-side test pins the URL of [section 3](#3-integrity) |
| moves or renames `.asc` | every fetch from 1.34.4 on: refused, naming the `404` | the refusal; `a_missing_signature_refuses_only_a_release_that_signs_every_asset` pins the URL and the version from which it is required |
| signs with a subkey or key the carried keyring lacks | every fetch of the releases it signs: refused, naming the primary key | the refusal; the answer is a new `trust/influxdata-archive.gpg`, which `the_carried_key_verifies_influxdatas_own_signature` checks against a signature InfluxData made |
| drops the SHA-256 table from its release notes, or changes its shape | every fetch of that release: refused, naming the release page | the refusal; `the_release_notes_digest_is_read_by_the_assets_name` pins the shapes of [section 3](#3-integrity) |
| moves or renames the program inside the archive | the install: refused, naming what the archive holds | the refusal on the first host; `the_program_is_taken_from_its_place_in_upstreams_archive` (client side) pins the layout of [section 5](#5-form-in-the-delivered-tree) against the kind's `MEMBER` |
| adds a second file at the program's place | the install: refused, naming both | the refusal; `a_member_held_twice_refuses_the_archive_in_every_container` pins it |
| stops reloading on `SIGHUP` | every apply, invisibly: the process keeps its old configuration | nothing — `telegraf_applies_a_configuration_by_sighup_in_place` shows the kind sends the signal, not that Telegraf acts on it, and this row is the warning |
| stops printing a plain SemVer version | `service.version`, and the preflight | nothing — the Agent reports no version |

The last two rows are honest rather than reassuring: a test can pin what this repository writes
down, not what a program does at run time.
