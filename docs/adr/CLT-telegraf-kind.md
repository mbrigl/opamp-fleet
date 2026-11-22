# CLT-telegraf-kind: Telegraf gets a kind of its own that reloads it the platform's way, delivered as upstream's single-file archive

- **Status:** 🟡 proposed
- **Date:** 2026-11-22
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-agent/src/supervisor/telegraf.rs`, its tests in `crates/fleet-agent/tests/telegraf_supervisor.rs`, the `telegraf` feature of `crates/fleet-agent/Cargo.toml`, `telegraf_plans`, `TELEGRAF_PLATFORMS`, the release-notes checksum and the `.asc` check of opamp-fleetctl package fetch in `crates/fleet-tools/src/fetch.rs`, the release-notes parser in `crates/fleet-tools/src/fetch/upstream.rs` and its fuzz target `fuzz/fuzz_targets/release_notes.rs`, `crates/fleet-tools/trust/influxdata-archive.gpg`, `config/examples/telegraf-conf.toml`, the `telegraf` entry of the grammar map in `crates/fleet-server/static/index.html`, and `docs/artifacts/telegraf.md`

## Context

The fleet manages InfluxData's Telegraf beyond the Collector. It should be an ordinary Managed
Process: a package the Client installs into its own Supervisor directory, updates with a health
gate and rolls back, a Configuration rolled out from the Server, a restart issued from it. A kind
knows its own agent ([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)):
what follows from the artifact and the platform is compiled in, and a block holds only what an
operator decides.

Telegraf is a single-file program. Its whole invocation is Telegraf's own: the program name,
`--config`, `--version`, and `SIGHUP` to re-read its configuration. The reload is the sharpest case
for a kind: no block declares a reload
([CLT-wrappers-act-through-the-core §7](CLT-wrappers-act-through-the-core.md)), so a `command`
block for Telegraf restarts it on every apply, the children of its `execd` plugins with it, where
`SIGHUP` would have it re-read its configuration in place.

Telegraf is not always one process. Its `exec` and `execd` inputs and outputs start programs the
configuration names, and `execd` keeps them running beside Telegraf. A Supervisor's bounded stop
that kills only Telegraf's pid leaves those children running
([CLT-wrappers-act-through-the-core §32](CLT-wrappers-act-through-the-core.md)).

InfluxData publishes Telegraf's binaries on a CDN, not on its GitHub releases. The SHA-256 of each
archive stands in the release notes on GitHub, and from 1.34.4 on an OpenPGP signature stands
beside every archive on the CDN. `opamp-fleetctl package fetch` reads every download within a cap,
and every parser of downloaded bytes has a fuzz target (Q-2).

The bundled UI colours an Agent type's effective configuration by the grammar its kind's ADR
names ([SRV-kinds-name-their-ui-grammar](SRV-kinds-name-their-ui-grammar.md)). Telegraf's
configuration language is TOML.

## Decision

We will supervise Telegraf with a two-line `telegraf` kind that reloads it the platform's way,
delivered as upstream's own archives.

1. **The `telegraf` kind knows the invocation and has no settings.**
   - The program is `telegraf` plus `EXE_SUFFIX`.
   - The arguments are `--config ${config_dir}/telegraf-conf`.
   - `--version` is the version probe and the preflight, read as strict SemVer.
   - The Agent type is `service_name = "telegraf"`.
   - The block is `type = "telegraf"` and `name`.
   - A block carrying `args`, `version_args`, `reload_signal`, `env` or `working_dir` fails at
     startup, naming what supplies the value.
   - So does a block restating a value the kind derives: its program key `command`, or
     `program_path` ([CLT-wrappers-act-through-the-core §20](CLT-wrappers-act-through-the-core.md)).

2. **Telegraf's reload is the kind's, platform-correct by construction.**
   - The kind applies a Configuration with `SIGHUP` on Unix.
   - The core applies it by restart on Windows, where the kind names no signal.
   - No block names either, so one Supervisor set serves a mixed fleet.

3. **Telegraf is a single-file package, installed as published.**
   - The archives are `telegraf-<version>_<os>_<arch>.tar.gz` from
     `https://dl.influxdata.com/telegraf/releases/`.
   - For Windows the archive is a `.zip`, opened as
     [PKG-signed-packages-and-delivery §62](PKG-signed-packages-and-delivery.md) decides.
   - Versions are the `v<major>.<minor>.<patch>` tags of `influxdata/telegraf`.
   - The CDN has no listing, so the platform list is the tool's own (`TELEGRAF_PLATFORMS`).
   - The list holds linux amd64, arm64 and 386, darwin amd64 and arm64, and windows amd64 and
     arm64. Upstream spells `386` as `i386`.
   - The list holds only the builds whose Platform is beyond doubt.
   - `linux_armhf` and `linux_armel` stay out. Upstream does not state the ARM level they are
     built for.
     [PKG-signed-packages-and-delivery §6](PKG-signed-packages-and-delivery.md) keeps `armv6` apart
     from `arm32`, so an `arm32` entry would be offered to a host it does not run on.
   - **The release notes on GitHub state the SHA-256.**
     - The tool reads the digest from the table in the release notes of the tag.
     - The tool finds the row by the archive's exact name.
     - The digest is required.
     - The tool refuses an archive the notes name nowhere. Otherwise only the host that serves it
       would vouch for it.
   - **Each archive carries InfluxData's signature.**
     - The tool verifies the `.asc` beside the archive as
       [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) verifies an
       upstream's OpenPGP signature.
     - The key is InfluxData's package-signing key, which the tool carries
       (`crates/fleet-tools/trust/influxdata-archive.gpg`).
     - The pinned primary key is `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927`.
     - The SHA-256 check stays.
     - From 1.34.4 on InfluxData puts an `.asc` beside every archive. There a `404` refuses the
       archive.
     - Before 1.34.4 a `404` is the signature's absence, and the SHA-256 decides.
     - An `.asc` that exists must verify, at any version.
   - **Every download is bounded (Q-2).**
     - The archive is held to the artifact cap of `package fetch`.
     - The `.asc` is held to 64 KiB.
     - The release object that carries the notes is held to the cap of an answer of GitHub's API.
     - The parser of the release object and of its notes has the fuzz target
       `fuzz/fuzz_targets/release_notes.rs`.
   - **The kind names where the archive holds the program.**
     - The `member` is `usr/bin/telegraf` on Unix and `telegraf.exe` on Windows, below the
       version-named directory.
     - [PKG-signed-packages-and-delivery §55](PKG-signed-packages-and-delivery.md) lets a kind
       name it.
     - The file name alone does not decide it. The Unix tarballs also hold
       `etc/logrotate.d/telegraf` and directories named `telegraf`.
     - The Client unpacks only the program, so the kind has no `program_path`.

4. **Telegraf does not speak OpAMP to the Client.**
   - The kind refuses an `endpoint_port`.
   - Its Endpoint is bound, and nothing connects.

5. **The Configuration name is part of the contract.**
   - `opamp-fleetctl package fetch` uploads `telegraf-conf` beside the package, with the body
     `config/examples/telegraf-conf.toml`.
   - It uploads it only when the Server holds none of that name. It never overwrites one and
     distributes nothing.
   - The name is the file name the kind points Telegraf at.
   - Both sides must spell the same literal. A rename on one side alone leaves Telegraf reading a
     file that does not exist.

6. **The kind has an artifact document held by two tests.**
   - `docs/artifacts/telegraf.md` states the eight sections of
     [CLT-wrappers-act-through-the-core §26](CLT-wrappers-act-through-the-core.md).
   - The sections are these:
     - source, assets, integrity and treatment;
     - the shape in the delivered tree, and what the Client derives;
     - the Configurations, and what goes red when upstream moves something.
   - One test holds the packing side, and one the client side. Each is `cfg`-gated per platform
     where the facts differ.

7. **Telegraf does not write the cgroup tree.**
   - Every system unit carries `ProtectControlGroups=yes`
     ([CLT-accounts-service-runs-under §8](CLT-accounts-service-runs-under.md)), which makes
     `/sys/fs/cgroup` read-only.
   - A kind of its own states of its agent whether that takes anything from it.
   - It takes nothing from Telegraf. Its `cgroup` input reads the files under `/sys/fs/cgroup`
     that its `paths` select, and writes none.

8. **Telegraf leads its own process group.**
   - The kind starts Telegraf with `own_process_group = true`
     ([CLT-wrappers-act-through-the-core §32](CLT-wrappers-act-through-the-core.md)).
   - The reason is the children Telegraf starts: the programs of its `exec` and `execd` inputs and
     outputs.
   - The core's stop then signals the group, so a kill leaves no such child running.
   - The reload signal still goes to Telegraf's pid alone, as for every kind. What Telegraf does
     with its children on a reload is its own.

9. **The UI colours Telegraf's effective configuration as TOML.**
   - This clause names the grammar that
     [SRV-kinds-name-their-ui-grammar §3](SRV-kinds-name-their-ui-grammar.md) asks a kind's ADR to
     name.
   - The kind needs no new grammar. TOML is one the bundle holds.

**Out of scope:**

- following InfluxData's key at run time, and a revocation its carried keyring does not state,
  which [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) leaves out;
- an account for Telegraf other than the one
  [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md) gives the Client or its
  second service, for which the kind has no key;
- what Telegraf collects and where it sends it, which is the operator's `telegraf-conf`.

## Latitude

None.

## Alternatives considered

- **A `command` recipe.** It runs on every platform, but no block declares a reload, so every
  apply restarts Telegraf and its `execd` children where `SIGHUP` would do. The recipe also repeats
  on every host what the artifact decides: the program's name per platform, `--config` and
  `--version`. Rejected.
- **One block with placeholders** (`${exe_suffix}` and friends). Rejected: it turns the operator's
  file into a template of the artifact's layout.
- **Let the kind accept `args`** as an escape hatch. Rejected: a kind that needs one does not know
  its agent; an operator's choices belong in the agent's own configuration, and `command` remains
  for a genuinely different invocation.
- **A generic "single-file agent" kind** parameterised by name. Rejected: its parameters would be
  the keys the `telegraf` kind removes — `command` under a shorter name.
- **A kind that drives the native service managers** (`systemctl`, `sc.exe`). Rejected: supervising
  a process the Client did not spawn means no watchdog, no health gate, no bounded stop.
- **Telegraf signalled as one process (`own_process_group = false`).** It is the default for a kind
  with one process. Telegraf's `execd` children, and an `exec` child running at the time, would
  outlive a kill.
- **The `.DIGESTS` file beside each archive as a second digest.** An earlier form of this draft
  required it to agree with the release notes, from 1.39.0 on. Wherever InfluxData publishes a
  `.DIGESTS`, it also publishes an `.asc` the tool requires, and the signature already holds the
  archive to InfluxData's key. The cross-check added a request, a parser and a refusal, and decided
  nothing the signature leaves open.

## Sources / Prior art

- [Telegraf configuration](https://docs.influxdata.com/telegraf/v1/configuration/) — `--config`,
  and reload on `SIGHUP`.
- The READMEs of Telegraf's `execd` input and output, `plugins/inputs/execd/README.md` and
  `plugins/outputs/execd/README.md` on the `master` branch of `influxdata/telegraf`, read on
  2026-11-22: each runs the program it names "as a long-running daemon" and restarts it after an
  unexpected termination.
- InfluxData's release archives at `dl.influxdata.com` with their `.asc` signatures, and the
  archives' layout, read from the 1.36.4 linux/amd64, darwin/arm64 and windows/amd64 archives on
  2026-10-25.
- InfluxData's package-signing key at `https://repos.influxdata.com/influxdata-archive.key`, read
  on 2026-10-28: primary `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927` (2023-01-18, no
  expiry), signing subkeys `9D53 9D90 D332 8DC7 D6C8 D3B9 D8FF 8E1F 7DF8 B07E` (expired
  2026-01-17) and `AC10 D744 9F34 3ADC EFDD C2B6 DA61 C26A 0585 BD3B` (2025-07-10 to 2029-01-17).
  [Package Signing Key Rotation](https://www.influxdata.com/blog/package-signing-key-rotation/)
  (InfluxData, updated 2026-01-06) names the same primary and both subkeys, and says new builds
  are signed with the new subkey.
- The `.asc` beside the linux/amd64 and windows/amd64 archives of every release tag, probed on
  2026-10-28: published for both from 1.34.4 to 1.40.2 without a gap, and for every platform of
  1.34.4, 1.35.0 and 1.40.2; before 1.34.4 only for the linux/amd64 archive of a few releases
  (1.13.4, 1.33.0, 1.34.1 among them). Every one read was made by the 2023 subkey up to 1.37.0
  and by the 2025 subkey from 1.37.1; `gpgv` verified those of 1.40.2 and 1.36.0 with the key
  above. On 2026-11-22 `gpgv` 2.2.40 verified the `.asc` of the 1.37.0 linux/amd64 archive, made
  by the expired 2023 subkey: it reported `EXPKEYSIG` and a `VALIDSIG` naming the pinned primary,
  and exited with status 0.
- The release notes of `influxdata/telegraf` on GitHub, read through the API on 2026-10-25 for
  1.20.0, 1.24.0, 1.28.0, 1.30.0, 1.33.0, 1.35.0, 1.36.3, 1.36.4 and 1.38.0: each carries a table
  of `<arch> | <os> | <asset as a link> | <sha256>` rows, the name and the digest in backticks
  in 1.20.0 and 1.24.0 and bare from 1.28.0.
- Telegraf's `cgroup` input, `plugins/inputs/cgroup/cgroup_linux.go` and its README on the
  `master` branch of `influxdata/telegraf`, read on 2026-11-15: the plugin reads the files its
  `paths` select with `os.ReadFile`, finds them with `os.Stat` and `filepath.Glob`, and writes
  nothing.

## Consequences

- Positive: one block for every platform and for a mixed fleet; the reload is right everywhere
  without the operator choosing, and keeps Telegraf's state across a configuration change where
  signals exist.
- Positive: the artifact travels exactly as upstream published it, verified against upstream's own
  digest.
- Positive: from 1.34.4 on an archive is held to InfluxData's own key as well, so neither GitHub
  nor the CDN, nor both together, can swap it unnoticed.
- Positive: a stop takes Telegraf's `exec` and `execd` children with it.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests.
- Negative: a fetch trusts GitHub's release notes and InfluxData's CDN together, and is refused
  for a release whose notes carry no SHA-256 table. It reads the release's notes where a
  Collector's fetch reads its assets, so a run makes as many requests against GitHub's
  unauthenticated rate limit as a Collector's: the tags and one release.
- Negative: before 1.34.4 most archives carry no signature, and the release notes alone vouch for
  them.
- Negative: the tool carries InfluxData's key and has to follow it: when InfluxData signs with a
  subkey the carried keyring lacks, every fetch of the new releases is refused until a release of
  the tool carries the new key. A fetch of 1.34.4 or later needs `gpgv` on the host that runs it,
  which Debian and Ubuntu install with `apt` and macOS does not install by default.
- Negative / trade-offs: **a remote `telegraf-conf` runs code on the host.** Telegraf's
  configuration language can run commands and write files: `inputs.exec`, `inputs.execd`,
  `outputs.exec`, `outputs.execd` and `outputs.file`. Whoever writes the `telegraf-conf` the Server
  delivers runs commands as the account Telegraf runs under, and writes files as it.
  - Under `--run-as root`, or with a `sudoers` rule for `use_sudo`, that is code run as root from
    the Server.
  - On a host where Q-5 has to hold against the Server, the operator lists the Supervisor in
    `[supervisors] remote_config_disabled`
    ([the hardening guide](../HARDENING.md#the-channels-that-put-code-on-the-host),
    [the Client manual](../manual/client.md#switching-remote-configuration-off-for-one-supervisor)).
    Telegraf then runs on the `telegraf-conf` the host holds.
  - Where a process account is registered
    ([CLT-accounts-service-runs-under §18](CLT-accounts-service-runs-under.md)), those commands run
    under that account, which is never root and cannot write `supervisor.toml`.
- Negative / trade-offs: Telegraf runs under the Client's account
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)), which a Linux package
  makes non-root, so an input that needs root fails there. The unit sets nothing that implies
  `no_new_privs` (CLT-accounts-service-runs-under §8), so `use_sudo` with a `sudoers` rule for the
  account works; `--run-as root` is the other way. Both widen what a remote `telegraf-conf` can do,
  as the item above says. Where a process account is registered, Telegraf runs under that account,
  and the rule names it. On Windows a first registration runs the service as its own virtual
  account `NT SERVICE\<PRODUCT_NAME>`, and a re-registration keeps the account the SCM holds,
  `LocalSystem` included (CLT-accounts-service-runs-under §1); Telegraf runs under that account,
  and `--run-as` chooses another. The kind has no key for either.
- Follow-ups: none.

## Enforcement

Tests that carry `Verifies: CLT-telegraf-kind`:

- In `crates/fleet-agent/src/supervisor/telegraf.rs`:
  - `the_block_has_no_settings` and `the_recipes_keys_are_refused_by_name` hold clause 1.
  - `restating_a_derived_value_is_refused` holds clause 1 for `command` and `program_path`.
  - `the_invocation_points_at_the_delivered_configuration` holds clauses 1 and 5. It asserts the
    literal `telegraf-conf`.
  - `the_defaults_are_the_artifacts` holds the client side of clause 6, and clause 4.
  - `the_program_is_taken_from_its_place_in_upstreams_archive` holds the archive's layout of
    clause 3.
  - `telegraf_leads_its_own_process_group_and_reloads_on_sighup` holds clause 8, and the
    manifest's side of clause 2.
  - `the_version_is_asked_for_with_version` holds the version probe, which is also the preflight.
  - `telegraf_acts_only_through_the_host` holds that the kind does no I/O of its own: every input
    reaches the host function of its name.
- In `crates/fleet-agent/tests/telegraf_supervisor.rs`:
  - `a_two_line_telegraf_block_runs_reports_and_applies` holds clause 1. The stub records the
    arguments it was started with, and the test reads `--config` and the entry's path from them.
  - `telegraf_applies_a_configuration_by_sighup_in_place` holds clause 2 on Unix.
  - `telegraf_applies_a_configuration_by_restarting_on_windows` holds clause 2 on Windows.
- In `crates/fleet-tools/src/fetch.rs`:
  - `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` holds the packing
    side of clause 6.
  - `the_release_notes_digest_is_read_by_the_assets_name` holds the digest of clause 3.
  - `an_archive_the_release_notes_name_nowhere_is_refused` holds the required digest of clause 3.
  - `the_carried_key_verifies_influxdatas_own_signature` holds the carried key and the pinned
    primary of clause 3.
  - `a_missing_signature_refuses_only_a_release_that_signs_every_asset` and
    `a_published_signature_that_does_not_verify_refuses_the_artifact_at_any_version` hold the
    signature of clause 3.
  - `a_signature_past_its_cap_is_refused` holds the bound on the `.asc` of clause 3.
  - `the_default_configuration_is_named_as_the_kind_reads_it` holds clause 5. It reads the
    literal on both sides.

Clause 9 is held by the test that
[SRV-kinds-name-their-ui-grammar](SRV-kinds-name-their-ui-grammar.md) names for its clause 3. That
test pins the map in `index.html`, the `telegraf` entry among them.

**Not mechanically decidable:**

- Whether upstream still reloads Telegraf on `SIGHUP` is a run-time property no repository test
  can see. The artifact document names this row as a warning.
- That Telegraf writes nothing under `/sys/fs/cgroup` (clause 7) is a property of its code. It is
  read from its source and not tested here.
- That `gpgv` accepts a signature by a subkey that has expired since it signed is shown by a probe
  named under Sources. A fixture needs an archive of about 79 MB.
