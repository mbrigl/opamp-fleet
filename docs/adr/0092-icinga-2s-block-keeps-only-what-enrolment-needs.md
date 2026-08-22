# ADR-0092: Icinga 2's block keeps only what enrolment needs

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Applies [ADR-0091](0091-a-kind-knows-its-own-agent.md) — *derivable means derived; a key exists only
for a decision* — to the `icinga2` kind of [ADR-0068](0068-icinga-2-is-supervised-by-a-kind-of-its-own.md).
The rule, the `Plugin` seam, the migration mechanism and the artifact-document requirement are
stated there and not repeated here.

## Context

**Eleven keys and an environment table, and most of it describes an artifact this project builds
itself.** A working Icinga block in `docs/manual/icinga2.md` reads:

```toml
binary = "icinga2"
program_path = "sbin/icinga2"
include_dir = "${supervisor_dir}/program/tree/share/icinga2/include"
plugin_dir  = "${supervisor_dir}/program/tree/plugins"
main_config = "icinga2-conf"
node_name   = "edge-01.example.com"
parent_host = "master.example.com"
parent_port = 5665
ticket_file = "${config_dir}/icinga2-ticket"
stop_timeout_secs = 60
apply_grace_secs  = 30
[supervisor.env]
LD_LIBRARY_PATH = "${supervisor_dir}/program/tree/lib"
```

Every path in it follows from the tree `opamp-package-fetch --agent icinga2` produces
([ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)), and the manual has to
add that *"on Windows the same block differs in three values"* — `icinga2.exe`, `sbin/icinga2.exe`,
and a `plugin_dir` pointing at `sbin`, because a Windows program finds its DLLs beside itself. Three
platform differences, transcribed per host.

**Five of the keys are already derived and exist only to be written again.** `Icinga2Plugin::layout`
defaults `data_dir`, `log_dir`, `cache_dir`, `spool_dir` and `run_dir` to `${supervisor_dir}/…`
today; the block key is a way to restate the default.

**What is left over is not the agent — it is the installation the host is joining.** The parent's
address, the ticket, the pinned certificate, and the name the ticket was issued for. Those cannot
be computed here, and this ADR does not try.

## Decision

We will reduce the `icinga2` block to **its enrolment**, and let the kind hold everything the
artifact and the platform decide.

1. **The kind supplies what the tree decides**, per platform: the program name (`icinga2` +
   `EXE_SUFFIX`), `program_path` (`sbin/icinga2[.exe]`), `include_dir`
   (`<tree>/share/icinga2/include`), `plugin_dir` (`<tree>/plugins`, and `<tree>/sbin` on Windows),
   `service_name` (`icinga2`), and `LD_LIBRARY_PATH` on Unix. The five state directories keep the
   values they already default to. Their keys are **removed**, on ADR-0091's terms: a block still
   carrying one fails at startup naming the derived value.

2. **`parent_port` folds into `parent_host`.** `master.example.com:5665`, defaulting to Icinga's
   5665. One address is one value; splitting it bought a key that was wrong in exactly one direction
   (a port without a host names no parent, which the old validation had to say out loud).

3. **`renew_before_days`, `run_as_user`, `run_as_group`, `log_level` go.** Renewal stays at 30 days.
   The account becomes the one this Client runs as — a Managed Process the fleet installed has no
   business under another. And logging belongs in Icinga's own configuration, where `object
   FileLogger` carries a `severity`: raising verbosity becomes a Configuration the fleet rolls out
   rather than a flag in one host's file. The decision keeps existing; it stops living in the TOML.

4. **`main_config` moves to the fleet, as a role.** It names which delivered Configuration is
   Icinga's root — the one file the daemon is pointed at, from which it `include`s the rest — and
   that cannot be derived from the entries: `icinga2-conf` and `icinga2-zones` are both delivered
   without a role, and being unroled says *"this is configuration"*, not *"this is the root"*. So
   the fleet says it with the field the Baseline provides for exactly this, in exactly these words:

   > Optional role of the content in the body field. **The values and their semantics are Agent
   > type-specific.**

   A vocabulary of one kind's own is therefore the field working as intended, not a corner of it
   being borrowed: `main` means *this* to `icinga2` and nothing to anyone else. The root carries
   **`role = "main"`** ([ADR-0016](0016-configuration-content-role.md), whose empty/`supplementary`
   pair stays the reading for kinds that define nothing further), and the kind takes that entry. Where nothing carries it, the conventional name `icinga2-conf` — what
   `opamp-package-fetch` uploads — is the fallback, so a rollout written before this ADR keeps
   working. The role stays interpreted per kind: to a Collector any non-empty role still means
   "written, never passed as `--config`".

5. **Four keys stay, and all four are the enrolment.** This kind supervises the **Agent role and
   never a master** (ADR-0068); these are not settings *of* a master but what the Agent must know to
   reach one, and every one of them is consumed on this side:

   | Key | What the Agent does with it |
   |---|---|
   | `node_name` | its own `NodeName`, the CN of its certificate, its Endpoint name |
   | `parent_host` | the address it dials, to enrol and then to connect |
   | `ticket_file` | the ticket it presents when asking for a certificate |
   | `trusted_cert_file` | the parent's certificate it pins, instead of trusting on sight |

   `node_name`'s **default improves**: the host's FQDN — Icinga's own convention (`hostname --fqdn`)
   and what an operator following Icinga's instructions feeds `pki ticket --cn` — rather than the
   Supervisor's name, which the instance-name grammar of ADR-0010 cannot even spell as an FQDN and
   which was therefore wrong on nearly every host.

   The FQDN is **resolved**, not taken from what this Agent already reports: `host.name` is
   `gethostname`, which the semantic conventions permit to be either form and which is the short
   name on most Linux hosts — a default that fails enrolment rather than merely reading oddly.
   `getaddrinfo` with `AI_CANONNAME` is the same route `hostname --fqdn` takes, and **only a name
   containing a dot is accepted**: an unqualified answer is what a resolver hands back for a host
   with no domain, and taking it would reintroduce the default this replaces. Resolved once per
   process, and only where no `node_name` is configured, so a slow resolver costs one lookup and an
   operator who states the name costs none. On Windows nothing is resolved and the old default
   stands — this kind is unproven there, and reaching for a platform API this crate does not
   otherwise use would be a cost paid for a case nobody runs yet. The key survives because a host whose master knows
   it under another CN must still be able to say so: a mismatch does not read oddly in a dashboard,
   **enrolment fails**.

   **All four are already rollable, which is why they need no mechanism of their own.** The
   `[[supervisor]]` blocks are the fleet-managed half of `supervisor.toml`
   ([ADR-0056](0056-the-client-accepts-its-supervisor-set-from-the-server.md)), so a Configuration
   for the Client's own Agent carries every one of them; and the two that are *files* travel as
   supplementary Configurations, exactly as the ticket reaches a host today (ADR-0069, ADR-0016).
   What this ADR changes is not whether they can be delivered, but how many of them have to be
   **written** — see the consequence about a fleet-wide Icinga set below.

6. **`args` and `env` go with the rest.** ADR-0091 keeps the escape hatch only on the kinds that
   know nothing about their agent. A wrapper that needed one would be a wrapper that does not know
   its agent — and where an operator genuinely needs to change how Icinga runs, Icinga's own
   configuration is the place, which the fleet already delivers.

7. **This kind's artifact document is `docs/artifacts/icinga2.md`**, in the shape ADR-0091 clause 9
   defines, pinned by the two tests it requires: one against the repack plan in
   `opamp-package-fetch` (the Debian repack, the MSI payload, the wrapper directory name, the
   output names), one against the constants above, `cfg`-gated per platform.

## Alternatives considered

- **Keep the paths as overrides with the derived values as defaults.** Nothing breaks, and a
  differently packed tree still works. Rejected on ADR-0091's general ground — two sources of truth
  for a value the Client computes — and on a specific one: the values are not *this host's*, they
  are the artifact's, so a per-host override can only ever be wrong or redundant.

- **Probe the unpacked tree for `sbin/icinga2*` instead of compiling the layout in.** It would
  survive a repacked tree. Rejected: a wrong guess is silent and lands on a host, while a constant
  is testable against the artifact this project builds and fails loudly when that artifact moves.

- **Require the root Configuration to be named `icinga2-conf`, full stop.** Simplest removal of
  `main_config`: no key, no role. Rejected: a fleet that does not use `opamp-package-fetch` names
  its Configurations itself, and nothing would be left to say which one is the root — the failure
  being a daemon pointed at a file that is not its configuration.

- **Derive `node_name` from the FQDN and drop the key.** Two lines for Icinga, like `glpi` and
  `telegraf`. Rejected: a host already enrolled under another CN could not be expressed at all, and
  the failure is a refused enrolment rather than a cosmetic mismatch.

## Sources / Prior art

- **Icinga 2, Distributed Monitoring** — `NodeName` *"should be set to FQDN which is the default if
  not set"*, the requirement that `NodeName`, the certificate CN and the Endpoint object name are
  the same string, and `icinga2 pki ticket --cn '<fqdn>'` as the way a ticket is minted
  ([docs](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/)).
- **Icinga 2, Configuration** — the constants the daemon takes as `-D`, and `object FileLogger` with
  its `severity`, which is where logging verbosity belongs
  ([docs](https://icinga.com/docs/icinga-2/latest/doc/04-configuration/)).
- **[ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)** — the tree whose
  shape this kind compiles in; **[ADR-0072](0072-the-windows-artifact-is-verified-by-its-publisher.md)**
  — the Windows payload, whose plugin location is the one platform difference that survives as a
  constant; **[ADR-0069](0069-the-icinga-master-signs-the-ticket-travels-as-a-configuration.md)** —
  the ticket as a Configuration, which is why `ticket_file` names a delivered file.
- **`crates/client/src/supervisor/icinga2.rs`, `Icinga2Plugin::layout`** — the five directory
  defaults this ADR turns from defaults into facts.

## Consequences

- **Positive: one Supervisor set can configure every Icinga host.** The blocks are fleet-managed
  (ADR-0056), but a fleet-wide one is only useful if every host may run the *same* block — and today
  it may not, because `node_name` differs per host and its default is never right. With the FQDN
  default it need not be written: one Configuration carries `parent_host` for everyone, the per-host
  ticket keeps arriving as its own supplementary Configuration, and what was one Configuration per
  host becomes one for the fleet.
- **Positive: the Windows block stops existing.** The three values the manual has to list separately
  become `cfg`-gated constants with a test behind them.
- **Negative: a differently packed Icinga tree no longer fits.** The answer is `opamp-package-fetch`,
  not a key — ADR-0091 clause 8 states the trade generally, and this is the agent where it bites
  first, because the tree is repacked from vendor packages rather than published as one.
- **Negative: a new role value is a convention the fleet has to get right.** `role = "main"` is read
  by this kind and ignored by the others, and an operator who renames the root Configuration *and*
  forgets the role gets a Supervisor that will not start. The message names both ways across — the
  role, or the conventional name — which is the least this trade deserves: what was a key on every
  host is now a field on one Configuration.

- **Negative: every existing Icinga block must be rewritten once**, and there is no block both
  versions accept — the old one requires `main_config`, the new one refuses it. The cutover per host
  is therefore unavoidable, which is why ADR-0091 makes the self-update rollback catch a
  configuration this version cannot read.
- **Follow-ups:** whether the four enrolment values should become concepts the **fleet** holds rather
  than values riding inside a rolled-out block — the ticket already travels that way (ADR-0069),
  and with the other three following, this block would be `type` and `name` alone.
