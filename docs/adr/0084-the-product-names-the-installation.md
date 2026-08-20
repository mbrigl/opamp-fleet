# ADR-0084: The product names the installation — one build-time name, one layout, one account

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

**What this document is about.** One thing names an *installation*, and this decides what that
thing is, what it reaches, and who owns what it reaches. The product's name is fixed at build time;
the install path is one level named after it; the service carries it; a second installation is a
second build. Two documents that decided parts of that same question are retired into it —
[ADR-0053](0053-the-linux-service-executes-from-opt.md), which decides which directory a Linux
system service executes from, and
[ADR-0062](0062-the-service-runs-under-an-operator-named-account.md), which decides which account
owns what it executes. Neither survives this decision's changes intact, which is why both are
carried here rather than left standing beside it.

Builds on [ADR-0082](0082-the-fleets-own-agent-is-called-supervisor.md), which states the naming of
the fleet's own agent whole, and supersedes three of its clauses: **clause 4** on the service's
name and its instance suffix, **clause 6** on the default instance name, and **row 1 of clause 9**,
which kept the install roots where they are. Everything else ADR-0082 decides — the Agent type, the
package that carries the Client, the release artifacts, the program's own name and its
configuration file — is untouched and is the ground this decision stands on.

Supersedes the **per-instance identity** decision of [ADR-0010](0010-client-os-service-and-cli.md)
— `--instance`, the per-instance install root, and the per-instance state directory. **Supersedes
[ADR-0053](0053-the-linux-service-executes-from-opt.md) whole**: its decision, the
SELinux derivation that forces it, the alternatives it rejected and the sources it rests on are
carried below — clause 3 and the two sections at the end of this document — with its paths
rewritten and its program/data split extended to Windows for a second and different reason. Nothing
of ADR-0053 is left behind for a reader to have to find. **Supersedes
[ADR-0062](0062-the-service-runs-under-an-operator-named-account.md) whole** for the same reason and
by the same method: `--run-as`, the passwordless Windows account forms, the ownership hand-over and
the account-must-exist refusal are carried into clause 12, restated for a layout that now has two
roots instead of one — which is the change that made ADR-0062's own wording untrue and is why it
could not simply be left standing. Amends
[ADR-0046](0046-a-release-ships-native-installers.md) on the installation directory's name — but
not its clause 5, whose "one directory for everything" survives clause 3 intact — and
[ADR-0048](0048-the-packaged-cli-is-a-symlink-through-current.md) on the paths and identities it
names, extending its `/usr/libexec` ownership line to the MSI's `Program Files` payload. ADR-0010's install layout, its `--root` flag and its restart policy are untouched, as is
[ADR-0030](0030-one-service-name-on-every-platform.md)'s clause 1 — the single-token label — which
this decision does not weaken but guarantees earlier.

**ADR-0082 must be accepted first.** This decision supersedes clauses of a document that is still
proposed; until that document is binding, so is nothing here.

**ADR-0053 is already marked superseded**, ahead of this document's acceptance and by the decision
of its author. That is deliberate and it leaves a gap worth naming: between now and this document's
acceptance, the rule that a Linux system-scope service executes from `/opt` is recorded in a
superseded ADR and a proposed one, and is binding in neither. **The rule itself does not lapse** —
the code implements it (`service::manager::default_layout_root`, `service::layout`), the `.rpm`
depends on it, and an enforcing host is what enforces it. What lapses is only the paper. The gap
closes when ADR-0082 and then this document are accepted, and nothing else should be built on
either document's authority until it does.

## Context

The Client installs under `<base>/opamp-fleet/client/<instance>` — a product level, a component
level, and an instance level. On every host anyone has installed, the last two are constants:
`client` and `default`.

**The instance level is a capability no delivery path can reach.** ADR-0010 made multiple instances
a requirement and gave them a runtime flag, but nothing that ships uses it. The `.deb` and `.rpm`
maintainer scripts call `service install` with no `--instance` and hard-code `default` in eight
places; the MSI never passes the flag at all. Every packaged installation is the default instance,
and a second one is reachable only by unpacking an archive and registering it by hand.

**Nothing enumerates instances, so the name is something an operator must remember.** There is no
`service list`; no code reads the parent of the per-instance directory. `service uninstall` with no
flag removes the default instance, and an operator who installed `--instance prod` must type
`--instance prod` again — the install prints the incantation precisely because nothing can recover
it later.

**And the flag does nothing at runtime.** `RunSpec` carries `config_path`, `state_dir` and
`service`; there is no instance field. `--instance` selects a service name and two default paths at
*install* time, and is inert in the process it registers. It is baked into every unit's command
line and read by nothing.

**The middle level contradicts the program's name.** ADR-0082 clause 4 makes the program
`supervisor` but leaves the install roots alone, for the reason its clause 9 gives: the state
directory holds the instance UID and the credential an operator typed, and moving it would make
every host a *new* Agent in the fleet view. That reasoning is correct on its own terms, and it is
why `/opt/opamp-fleet/client/default/current/supervisor` spells two names for one thing.

**What has changed is the cost.** [ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) wrote
the rule this decision depends on: the cost of a rename "is entirely a function of when it happens,
and right now it is zero." For the *program* name that window had closed by the time ADR-0082 was
drafted — fourteen releases were out. For the *paths* it has not: no host has been installed. There
is no instance UID to strand, no credential to lose, no unit whose command line must keep parsing.
Every cost this decision would otherwise carry is a migration cost, and there is nothing to
migrate. The window closes with the first real installation, and clause 8 says so in as many words.

## Decision

We will fix the product's name at **build time**, collapse the install path to a single level named
after it, split program from data wherever the platform gives a reason, remove `--instance`, and
make a second instance a **second build**. Clauses 3 and 12 additionally carry, restated for that
layout, the two decisions this document supersedes whole: where a Linux system-scope service
executes from (ADR-0053) and which account owns what it executes (ADR-0062).

1. **`PRODUCT_NAME` is a build-time constant**, default **`opamp-fleet`** — the repository's own
   name — overridable with `OPAMP_FLEET_PRODUCT_NAME` for a variant build. It follows the grammar
   ADR-0010 set for instance names, for the same reason: it must be simultaneously a systemd unit
   name, a launchd label, an SCM name and a directory name. Lowercase `[a-z0-9-]`, 1–32 characters,
   no leading or trailing `-`, and never a Windows reserved device name. The build **fails** on a
   name that breaks the grammar, so an illegal name cannot reach a host.

2. **The install path is `<base>/<PRODUCT_NAME>`, on every platform.** The directory is named by
   the product, never by its display name: `/opt/opamp-fleet`, `/Library/Application
   Support/opamp-fleet`, `%ProgramData%\opamp-fleet`, and — where the MSI lays its payload down —
   `C:\Program Files\opamp-fleet`. It does not ask for prose in a directory name, and `nodejs`,
   `Git` and `PowerShell` sit under `Program Files` under their own names. `OpAMP Fleet Agent` is a
   display name and keeps the three places display names belong: the Add/Remove Programs entry, the
   SCM's display column, and the installer dialog's title. A directory named for the display name
   would also be the one place a variant build could not tell itself apart, since two variants
   differ in `PRODUCT_NAME`.

3. **Program and data split wherever the platform gives a reason, and the reason differs.** On
   Linux at system scope the executable layout is `/opt/opamp-fleet` and the data root
   `/var/lib/opamp-fleet`. **Linux at system scope is the only place that splits.** macOS, Windows
   and every user scope keep one directory, because no other platform gives a reason to. The reason
   is stated in full here because this decision retires the document that held it.

   **Linux: a binary under `/var/lib` is one SELinux never lets systemd start.** This was ADR-0053's
   decision and is now this one's, carried whole with its paths rewritten:

   - Fedora and RHEL run the SELinux targeted policy in **enforcing** mode by default, and
     openSUSE Leap 16 / SLES 16 have switched from AppArmor to exactly that. This is not one
     distribution family's quirk but the default posture of the entire rpm world the `.rpm` exists
     for.
   - Files created under `/var/lib` carry the type `var_lib_t`. systemd (`init_t`) may only
     `execve` types the policy marks as service entrypoints — `bin_t`, `usr_t` and friends,
     through which a third-party service transitions into `unconfined_service_t`. `var_lib_t` is
     not such a type, for anybody: data directories are deliberately not executable by the init
     domain.
   - The failure is deferred and silent at install time. The package installs, `service install`
     stages the layout and registers the unit, `systemctl enable` succeeds — and the first
     `systemctl start` dies with `status=203/EXEC` (Permission denied), an AVC denial in the audit
     log the unit's own journal never explains. The same binary runs fine from an interactive shell
     (`unconfined_t` may execute nearly anything), which makes the diagnosis actively misleading.
   - **Why `/opt` is the answer and not a label fix.** Its default file context is `usr_t`, an
     entrypoint type through which systemd transitions a third-party service into
     `unconfined_service_t` — the mechanism the targeted policy provides precisely so vendor
     software outside the distribution's packages can run enforcing. Files the self-update
     (ADR-0020) stages later inherit the directory's label, so staging keeps working with no
     SELinux tooling, no policy module and no new runtime dependency. This is load-bearing: the
     layout is rewritten at runtime by the *daemon*, so any fix that is applied once at install
     time is a fix that expires at the next staged version or the next filesystem relabel.
   - FHS 3.0 sanctions the shape rather than merely tolerating it: `/opt` is for add-on
     application software (§3.13) and an add-on package's variable data belongs under `/var`. The
     split is the standard-conformant layout, not a compromise, and it is the field-proven one —
     Elastic Agent, whose versioned-directory scheme ADR-0010 adopted, installs to
     `/opt/Elastic/Agent`.
   - **The blind spot that hid this is still on record.** The service smoke test excludes "hosts
     with SELinux or AppArmor in the way" from coverage (`crates/client/tests/service_smoke.rs`),
     which is why no automated check ever met an enforcing host. Superseding ADR-0053 does not
     close that gap and must not be read as having closed it.

   **Windows does not split, and `Program Files` is the MSI's payload directory — not the
   layout.** This is the same line ADR-0046 and ADR-0048 drew on Linux, applied to the platform
   that has the identical shape under different names. There, the `.deb` and `.rpm` deliver one
   file to `/usr/libexec/<PRODUCT_NAME>` — package-manager-owned, never rewritten — while the
   versioned layout under `/opt` belongs to the program. Here, the MSI delivers its payload to
   `C:\Program Files\<PRODUCT_NAME>` — `TrustedInstaller`-owned, meant to be read-only once
   installation finishes — and then runs the same `service install` the command line would, which
   builds the layout and the state directory under `%ProgramData%\<PRODUCT_NAME>`.

   **Putting the layout in `Program Files` was considered and is refused**, for the reason ADR-0053
   used to refuse `/usr/lib` and `/usr/libexec`: the layout is rewritten at runtime by the daemon,
   and the installer's hierarchy is the installer's. Clause 12 makes the cost concrete — the
   self-update means the service's own account must be able to write `versions/` and `current`, so
   a layout in `Program Files` would require granting a low-privileged service account modify
   rights on a tree it also executes from. That is a privilege-escalation surface offered in
   exchange for a directory listing, and this decision declines it.

   **What this settles.** A Windows host installed by the MSI and one unpacked from the archive now
   put the same things in the same places, which they did not before. And `service uninstall`
   deliberately leaves the root and the state directory behind — deleting a `supervisor.toml`
   holding a credential an operator typed would be the overwrite ADR-0027 refused, one step later —
   while the MSI's uninstaller empties `INSTALLFOLDER`. With `INSTALLFOLDER` holding nothing but
   the delivered payload, emptying it is exactly right, and the credential sits in `%ProgramData%`
   where Windows expects data to survive: the same remove-versus-purge shape the `.deb` already
   has.

   **This requires a second flag, on one platform.** `--root` keeps the meaning ADR-0053 gave it —
   everything under the one directory the operator names, whose labelling and permissions are then
   the operator's business, documented in the manual — and a new `--data-root` names the other. It
   exists for the Linux system-scope split and for an operator who wants the two halves apart
   anywhere; the MSI does not pass it, because Windows no longer splits. **ADR-0046 clause 5 is
   therefore left intact** rather than amended: `INSTALLFOLDER` stays "one directory for everything
   … the operator configures one path, not half of one", and what changes is only what that one
   directory holds — the payload, not the layout.

   **What a package removal takes, in the split layout.** ADR-0053 redrew ADR-0048's remove/purge
   distinction across the two roots and that redrawing is carried here, one level shallower:
   `postrm` on **remove** deletes the layout root — it holds nothing but staged binaries — and on
   **purge** additionally the data root. ADR-0048's mechanism is untouched; only the paths it names
   change, per this decision's amendment of it. ADR-0053's third packaging obligation is *not*
   carried, because clause 8 empties it: there is no orphaned `versions/` or `current` under an old
   `/var/lib` default root to clean up, and no legacy instance directory to `rmdir`, on a host that
   was never installed.

4. **`--root` still overrides, and is still never a fixed path.** Given alone it collapses layout
   and data into the one directory named, exactly as today.

5. **The service is `PRODUCT_NAME`, with no suffix.** ADR-0030's clause 1 stands unchanged: a
   single-token `ServiceLabel`, so systemd, launchd and the SCM render the same string.

   This supersedes **ADR-0082 clause 4's second sentence**, which made the service the *program's*
   name "with the instance suffixed as before (`supervisor-prod`) — ADR-0030's rule with its string
   replaced". With `--instance` removed there is no instance to suffix; and with one installation
   per build, the name that identifies an installation is the product's, not the program's. The
   rest of ADR-0082 clause 4 is untouched: the binary, the version directories, the archive member,
   the `/usr/libexec` payload, the log file, the self-check token and the CLI's own name all remain
   `supervisor`. Only the `PATH` symlink moves to the product name, because two variants would
   otherwise collide on one `/usr/bin` entry.

6. **The display name is `OpAMP Fleet Agent`**, set from a second build-time variable
   (`OPAMP_FLEET_PRODUCT_DISPLAY_NAME`): it is prose, not a slug, and cannot be derived from
   `opamp-fleet` by any rule that would still read correctly for the next variant.

   This supersedes **ADR-0082 clause 6's default**, `Supervisor Agent`. That clause's *reasoning* is
   adopted wholesale and is why the new default is not `opamp-fleet`: a default equal to the Agent
   type would print the same word in both columns of the fleet view, which is the collapse
   [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) ended. Its closing
   sentence loses one subject — the ADR-0010 grammar now governs the `[[supervisor]]` block names
   and, per clause 1, `PRODUCT_NAME`; `--instance` is gone.

7. **`--instance` is removed.** Not hidden, not accepted-and-ignored: removed, along with the
   per-instance root, the per-instance state directory, and the `--instance` argument baked into
   every registered unit. `service uninstall|start|stop|status` lose their lookup key and take no
   name at all — with one service name per build there is nothing to look up. The instance-name
   grammar itself **survives**, because `[[supervisor]]` block names still need it.

8. **The state directory moves, once, and only because there is nothing in it.** Two rules say it
   must not. ADR-0053's third decision bullet — *"The upgrade on a managed host is a
   re-registration, not a migration … identity, credential, and state never move"* — and row 1 of
   ADR-0082's clause 9, which kept `/opt/opamp-fleet/client/<instance>` and
   `/var/lib/opamp-fleet/client/<instance>` because *"the state directory holds the instance UID and
   the configuration; moving it makes every host a new Agent in the fleet view and loses the
   credential an operator typed"*. Both are correct, and this decision moves both roots anyway.

   **What discharges them is that the set of affected hosts is empty.** Fifteen `version/*` tags
   exist and no host has been installed from any of them. There is no instance UID on disk to
   strand, no Server-issued credential to lose, no `supervisor.toml` an operator typed an endpoint
   into, and no registered unit whose `--state-dir` argument must keep parsing. The hazard both
   rules name is a *migration* hazard, and it is discharged by there being nothing to migrate — not
   by being weighed against the benefit and found lighter. Were one host installed, this clause
   would not be available and the paths would stay where ADR-0082 left them.

   **The rule reinstates itself at the first installation, and this decision now owns it.** With
   ADR-0053 superseded, its third decision bullet is restated here as binding in its own right:
   from the first host installed under `<base>/<PRODUCT_NAME>`, **an upgrade on a managed host is a
   re-registration, not a migration** — `--config` and `--state-dir` keep pointing into
   `/var/lib/<PRODUCT_NAME>`, only `ExecStart` changes, and identity, credential and state never
   move. Any later decision that would move them again inherits the full weight of that rule and
   cannot borrow this clause's justification — the window this clause uses closes the moment it is
   used.

9. **The program and the Agent type keep their own names.** The binary is `supervisor`
   (`supervisor.exe` on Windows) and its configuration `supervisor.toml`, exactly as ADR-0082
   clauses 4 and 5 decided, and neither is derived from `PRODUCT_NAME`. Three names now sit side by
   side, each naming a different thing:

   | name | names | appears in |
   |---|---|---|
   | `opamp-fleet` | the **product** | the path, the service, the package, the `PATH` symlink |
   | `supervisor` | the **program** | the file, its configuration, the archive member |
   | `supervisor` | the **Agent type** | `service.name` on the wire, the package Set's key |

   The last two share a string and are separate constants. Keeping the program's name off
   `PRODUCT_NAME` is what lets **one published package Set serve every variant**: the archive member
   a self-update extracts is the same in all of them, so ADR-0082 clause 2's default — the Set is
   `supervisor` @ version @ `supervisor` — needs no per-variant exception.

   The Agent type constant is renamed from `CLIENT_SERVICE_NAME` to `CLIENT_AGENT_TYPE`. It never
   named a service, and with the service now carrying the product's name, the old name would read
   as the one thing it is not.

10. **A second instance is a second build**, with its own `PRODUCT_NAME` and therefore its own
   service name, `.deb`/`.rpm` package name, `/usr/libexec` payload directory, `PATH` symlink and
   MSI `UpgradeCode`. What was a runtime flag no delivery path could reach becomes a build variant
   every delivery path carries.

   This answers a question **ADR-0046 clause 5** deferred — it installed "the `default` instance
   only", noting that enumerating instances "would need a product code per instance, which is a
   different decision and not one anyone has asked for". This is that decision, answered
   affirmatively. The `UpgradeCode` is the one identity that cannot be derived: each variant needs a
   GUID minted once and recorded, because Windows Installer treats a shared `UpgradeCode` as grounds
   to remove the other installation. ADR-0046's own "minted once and never changed" discipline holds
   per variant.

   It also redraws the line **ADR-0048's sixth bullet** drew, which read `--root` or `--instance` as
   the mark of a manual install "where no package writes to `/usr/bin` or deletes a layout at all".
   Half that premise is gone: a non-default variant is now precisely a *packaged* install that does
   both. The `--root` half stands.

11. **Isolation is unchanged in kind.** ADR-0010 made an instance a boundary — separate Server,
   credentials, lifecycle, rollback — and that boundary survives intact; only the mechanism moves
   from a flag to a build. Scaling the number of *managed* Agents remains what it always was: the
   multiplexing of [ADR-0003](0003-client-modes-and-connection-multiplexing.md) inside one
   installation. With the service no longer suffixed, two variants on one host are told apart by
   `service.instance.name` — which is what ADR-0033 point 2, restated by ADR-0082 clause 6,
   already prescribes.

12. **The system service may run under an operator-named account, and both roots belong to it.**
   This is ADR-0062's decision, carried whole. It is here rather than in a document of its own
   because its load-bearing sentence — *whatever account the service runs as must be able to write
   the executable layout, because ADR-0020's updater is the daemon itself* — is a statement about
   the layout, and clause 3 has just changed how many layouts there are and where they stand.

   `service install` takes **`--run-as <account>`**, system scope only (it conflicts with `--user`),
   and without it the service registers exactly as it does today: root under systemd and launchd,
   `LocalSystem` under the SCM.

   - **The service runs as the account.** Linux: systemd `User=<account>`; macOS: launchd
     `UserName`; both through the `username` field `service-manager` already carries. Windows: an
     `sc config obj=` step in `windows_config` — the same "finish what the crate omits" seam the
     recovery actions already use — sets the logon account. No *Log on as a service* grant is
     performed: the default security policy grants that right to `NT SERVICE\ALL SERVICES`, which
     covers the virtual account; the built-in accounts carry it inherently; and a gMSA receives it
     from its domain's group policy. A host hardened to remove the default grant must restore it
     for this service's account, and the manual says so.
   - **Windows accepts only passwordless account forms**: the service's own virtual account
     (`NT SERVICE\<service name>`, the recommended form — and per clause 5 that name is now
     `PRODUCT_NAME`), a gMSA (`name$`), or `NT AUTHORITY\LocalService`/`NetworkService`. A password
     parameter does not exist, for ADR-0046's reason: it would stand in the process list and the
     installer log. An account form that needs one is refused with a message naming the passwordless
     forms.
   - **The account must already exist** on Linux and macOS, and the install refuses early — before
     anything is written, per ADR-0010 — with a message showing the one-line `useradd --system`
     that creates it. Creating accounts is packaging's business, not this binary's. On Windows the
     virtual account exists implicitly with the service.
   - **`uninstall` still deletes nothing**, and re-running `install` with a different `--run-as`
     re-owns the same directories.

   **The hand-over now names two roots, and this is the substantive change to ADR-0062.** That
   document spoke of "the instance's configuration and state directories" and of "the executable
   layout (`versions/` and `current`)" as things in one place, and its Context said in as many words
   that "on Windows the layout lives under `%ProgramData%`". After clause 3 neither is reliably
   true. Restated: after laying out and registering, the install hands ownership of **everything
   under the data root** — `supervisor.toml` and `state/`, because the service must read and write
   them — **and of `versions/` and `current` under the layout root**, because the updater is the
   service itself. `chown` on Unix, an ACL modify-grant on Windows. Where the two roots coincide,
   this is one operation; where they split, it is two, and an install that can perform only one of
   them has not succeeded.

   **This clause is why clause 3 keeps the Windows layout out of `Program Files`.** The hand-over
   has to reach `versions/` and `current`, so wherever the layout stands, the service's account
   must be able to write it. Under `Program Files` that would mean granting a low-privileged
   account modify rights on a `TrustedInstaller`-owned tree the service also executes from — a
   privilege-escalation surface, and the Windows form of exactly what ADR-0053 refused when it
   declined to put a self-rewritten layout under `/usr/lib` or `/usr/libexec`. With the layout
   under `%ProgramData%\<PRODUCT_NAME>` the grant lands where per-machine mutable data belongs, and
   `--run-as` works on a Windows host installed by the MSI exactly as on one unpacked by hand.

## Alternatives considered

- **Leave the path as it is.** Defensible while the contradiction is only cosmetic, and it was the
  right answer as long as hosts were installed. With none installed, it trades a permanent
  three-level path and a dead flag against no saving at all — and the option expires the day
  someone installs.

- **Collapse the path but keep `--instance`.** The smallest change, and it fixes the naming
  contradiction. But it keeps a flag that no package sets, nothing enumerates, and the runtime
  ignores, while requiring the instance to stay in the path to keep two installations apart. It
  preserves the cost of the capability without making the capability reachable.

- **Absorb ADR-0080 and ADR-0082 into this ADR.** Both are proposed, so nothing external references
  them and process rule 6's immutability would not be engaged. Rejected — and the contrast with
  ADR-0053 and ADR-0062, which this decision *does* supersede whole, is the reason. Each of those
  turns on a fact this decision changes. ADR-0053 decides which directory the Linux system-scope
  layout stands in; clause 3 rewrites those paths and clause 8 restates its migration rule, leaving
  nothing in it that is not said here. ADR-0062 decides which account owns that layout, and its
  Context asserts as fact that on Windows the layout lives under `%ProgramData%` — an assertion
  clause 3 falsifies. Leaving either standing would leave a second, stale address for a question
  this document has already answered differently. ADR-0082 is the opposite case: nothing in it is
  falsified here, and its subject is not this one. Its clauses reach the Agent type, the
  self-update package default, the release container and its field separators, the OTLP
  instrumentation scope and the CHANGELOG policy for breaking changes — an installation is not what
  any of those are about, and absorbing it would make this the place that decides why release
  artifacts are `.tar.gz`. ADR-0082 was itself written to consolidate three documents into one;
  superseding three of its clauses is the sanctioned way to change part of it, and leaves the rest
  where a reader will look for it.

  **The line this draws is the one that keeps this document readable**, and it is worth stating
  because two absorptions in one decision look like an appetite for more. A document is carried
  here when this decision makes it untrue, not when it merely sits nearby: ADR-0010's service verbs
  and restart policy, ADR-0046's installers and ADR-0048's `PATH` symlink are all adjacent to this
  subject and all keep standing, amended where their paths and names moved and no further.

- **Derive the program's name from `PRODUCT_NAME` too.** Tempting for consistency: a variant's
  binary would be `opamp-fleet-b`, visible as such in `ps`. It breaks the self-update. The archive
  member is extracted by name, so every variant would need its own published Set of the same
  bytes — the fleet would carry N products where it has one. Rejected in favour of clause 9.

- **Rename the program to `agent`.** Considered while choosing clause 9. `agent` is this system's
  word for *every* managed thing; the program that supervises the others cannot also be the generic
  term for them without reintroducing the collapse ADR-0033 ended. `supervisor` already says which
  Agent it is.

- **Keep the name a runtime value read from configuration.** Then it cannot name the directory the
  configuration is read from, and `service uninstall` needs the file to find the service it must
  remove. A name that identifies an installation has to exist before the installation is read.

The four alternatives **ADR-0053** weighed for the Linux split are carried with it, unchanged in
force — each is a way to keep executing from `/var/lib`, and each is still rejected:

- **A persistent SELinux file context from the scriptlets** (`semanage fcontext -a -t bin_t …` +
  `restorecon -R`) — needs `policycoreutils-python-utils`, which the `.rpm` would have to require
  or guard; a guarded fallback fails silently, which is this bug with extra steps. It is label
  management in shell, re-run after every self-update and every relabel, to keep executing from a
  directory the policy says should not be executed from.
- **Ship an SELinux policy module in the `.rpm`** — the most packaging-orthodox answer and the
  heaviest: a policy to author, build and verify across Fedora, RHEL and the newly-enforcing SUSE
  family, for a Client whose need is fully met by standing in the right directory. It remains the
  natural follow-up if a confined domain is ever wanted (CIS Server Level 2 flags
  `unconfined_service_t` daemons); nothing here forecloses it.
- **`chcon` at staging time** — not persistent across an autorelabel (`/.autorelabel`,
  `restorecon`), and it puts SELinux-specific tooling into the Client's own runtime path on every
  distribution, enforcing or not.
- **The executable layout under `/usr/lib` or `/usr/libexec`** — the labels would work, but the
  layout is application-owned and rewritten at runtime by the self-update, and ADR-0046/0048 drew
  the ownership line exactly there: the package manager's hierarchy is the package manager's.
  `/opt` is the FHS home for software a distribution's package manager does not own.

ADR-0053's fifth alternative — **move the whole root, state included, to `/opt`** — is the one
this decision no longer rejects for ADR-0053's reason. That reason was the fleet-wide state
migration it would force; clause 8 discharges exactly that hazard. It stays rejected on the
remaining ground alone: it parks variable data in `/opt` against FHS, and it would give back the
`Program Files` problem clause 3 solves on the other platform.

The six alternatives **ADR-0062** weighed for clause 12 are carried with it, unchanged in force:

- **Status quo (root / `LocalSystem` only)** — refused; least privilege is the requirement, and
  every comparable fleet agent has grown this knob.
- **systemd `DynamicUser=` / `StateDirectory=`** — Linux-only with no launchd or SCM analogue, and
  its ephemeral UIDs fight an installation whose identity, credential and state must persist across
  restarts. ADR-0010 bakes absolute paths for exactly that reason, and clause 8 reinstates it.
- **Arbitrary Windows accounts with a password** — the password stands in the process list and the
  installer log; ADR-0046 refused precisely this, and the passwordless forms cover the fleet cases.
  Elastic went the other way, a created local user with a managed password, at the cost of password
  machinery the virtual account gets from the OS for free.
- **A layout owned by root with self-update disabled under `--run-as`** — keeps "the service cannot
  replace its own binary" as a boundary, but breaks the self-update for exactly the installs the
  flag exists for. The specification wins.
- **A privileged updater helper** — a small root service that swings `current` on request. A second
  service, an IPC surface and a privilege boundary to defend, for one flag. Rejected as a present
  need; it remains conceivable as a future hardening ADR, and it is also the other possible answer
  to the `Program Files` tension clause 12 records.
- **Creating the account inside `service install`** — platform-specific user management in this
  binary (three APIs, three idempotency stories) that packaging does in one `postinst` line.
  Deferred to packaging.

## Sources / Prior art

- **ADR-0028's own rule on timing** — that a rename's cost is a function of when it happens — is
  the load-bearing precedent here, applied to paths rather than the program name, and read against
  a tag list that carries no installations.
- **Elastic Agent** and **Telegraf** ship one product name per package and register a service named
  after it; multiple instances are multiple installations, not a flag. Telegraf's `--service-name`
  exists precisely because its packaging cannot express a second one.
- **Windows Installer's `UpgradeCode` semantics** (`MajorUpgrade`, `FindRelatedProducts`,
  `RemoveExistingProducts`) fix the rule in clause 10: coexistence requires distinct identity, and a
  shared `UpgradeCode` is an instruction to replace, not to install beside.
- **The `service-manager` crate's label rendering**, already documented in ADR-0030, is why the
  grammar in clause 1 is the intersection of four naming rules rather than any one of them.
- **`cargo-deb` variants** (`[package.metadata.deb.variants.<name>]`) and `cargo-generate-rpm`'s
  `--set-metadata` are the mechanisms that let one source tree emit differently-named packages
  without templating the manifest.

The sources under **ADR-0053** are carried with its decision, since clause 3 now rests on them:

- systemd `status=203/EXEC` under SELinux — the failure signature, and why the same binary runs
  interactively (`unconfined_t`) but not as a service:
  <https://thomaspowell.com/2026/04/03/the-selinux-203-exec-systemd/>; a real-world case of a
  service binary in a data directory failing exactly this way (GitHub Actions runner):
  <https://github.com/actions/runner/issues/1606>.
- `unconfined_service_t` — the targeted policy's mechanism for third-party services started by init
  from entrypoint-typed files (Dan Walsh, its author):
  <https://danwalsh.livejournal.com/70577.html>; Red Hat's documentation of unconfined process
  domains:
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/selinux_users_and_administrators_guide/sect-security-enhanced_linux-targeted_policy-unconfined_processes>;
  CIS Server Level 2 flagging unconfined daemons (why a policy module stays a possible follow-up):
  <https://access.redhat.com/solutions/6714611>.
- openSUSE Leap 16.0 / SLES 16 release notes — SELinux targeted policy, enforcing by default,
  replacing AppArmor:
  <https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/html/release-notes-leap-160/index.html>.
- FHS 3.0 — `/opt`: add-on application software packages (§3.13), with variable data placed under
  `/var`: <https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch03s13.html>.
- Elastic Agent installs to `/opt/Elastic/Agent` on Linux (`--base-path` to override) — the same
  layout lineage ADR-0010 cites, standing where this decision keeps it:
  <https://www.elastic.co/docs/reference/fleet/installation-layout>.
- **Microsoft's `Program Files` / `ProgramData` guidance** is the Windows counterpart and the
  reason the second split has a different cause: per-machine application data that changes at
  runtime belongs under `%ProgramData%`, and `Program Files` is `TrustedInstaller`-owned and
  read-only to everything else after installation.

The sources under **ADR-0062** are carried with clause 12:

- [`service-manager` changelog](https://docs.rs/crate/service-manager/latest/source/CHANGELOG.md) —
  `ServiceInstallCtx.username`, honoured for systemd and launchd only; Windows explicitly left open,
  which is why clause 12 names a second SCM step.
- [Microsoft: Service User Accounts](https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts)
  and [virtual accounts](https://docs.delinea.com/online-help/privilege-manager/install/upgrades/virtual-accounts.htm)
  — `NT SERVICE\<name>` accounts are provisioned per service with OS-managed passwords.
- [Microsoft: `sc.exe config`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-config)
  / [`ChangeServiceConfig`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfiga)
  — setting the logon account does not itself grant the *Log on as a service* right; the default
  security policy's grant to `NT SERVICE\ALL SERVICES` is what covers virtual accounts.
- [Elastic Agent: unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged)
  — the same layout lineage again, installed by root but *running* as a dedicated
  `elastic-agent-user` that owns the agent's files, upgrades included.
- [OpenTelemetry Collector Linux packages](https://opentelemetry.io/docs/collector/install/binary/linux/)
  — the `.deb`/`.rpm` create a dedicated `otelcol` system user and run the unit with `User=`.

## Consequences

- **Positive: the layout says what it is.** `/opt/opamp-fleet/current/supervisor` — the product,
  then the program. No level asserts `client` while the file says `supervisor`, and no level exists
  solely to hold the constant `default`.

- **Positive: an illegal name fails the build, not the host.** The grammar moves from a runtime
  parse of operator input to a compile-time check, so the class of failure ADR-0010 guarded against
  cannot reach a service manager at all.

- **Positive: a large amount of code stops existing.** The maintainer scripts lose every migration
  branch — the legacy unit retirement, the `client.toml` gate, the ADR-0053 cleanup, the
  `opamp-fleet-client` symlink removal and the `rmdir` of two shared parents — because there is
  nothing installed to migrate. The service verbs lose their lookup key.

- **Positive: one Set updates every variant**, a direct consequence of clause 9, and the reason the
  Agent type must stay off `PRODUCT_NAME`.

- **Positive: one product, one layout, however it was installed.** A Windows host set up by the MSI
  and one set up from the archive now put the same things in the same kinds of place, and a
  credential left behind by an uninstall sits where Windows expects data to survive rather than in a
  directory that is supposed to be gone.

- **Negative: `service install` grows a flag.** `--data-root` is one more thing to get wrong, and
  it is the flag that has to be right on exactly the platform where getting it wrong is silent:
  a Linux system-scope install that names only `--root` collapses both halves into a directory the
  operator then owns the labelling of. The MSI is spared — it passes neither root and needs no
  second property — so `tests/msi_exe_command.rs` keeps parsing one path, not two.

- **Negative: the MSI's directory changes twice over.** `C:\Program Files\OpAMP Fleet Client`
  becomes `C:\Program Files\opamp-fleet`, *and* it stops being the install root — it now holds
  only the delivered payload, with the layout and the state directory under
  `%ProgramData%\opamp-fleet`. Nothing is installed, so nothing moves, but an operator who has read
  the current manual will look in the wrong place twice, and the screenshots and worked examples
  change with it.

- **Negative: multi-instance becomes a build-time decision.** An operator who wants a second
  installation can no longer get one by passing a flag; someone must produce a variant build. This
  is a real reduction in what a single artifact can do, accepted because the flag was unreachable
  from every artifact we actually ship — the capability is being moved to where it can be
  delivered, not removed, but it does become less immediate.

- **Negative: each variant costs a minted `UpgradeCode` and a package-name entry**, and both are
  manual and permanent. A forgotten `UpgradeCode` does not fail loudly at build time; it fails on a
  Windows host by removing the other installation.

- **Negative: two constants hold the string `supervisor` for different reasons.** The rename to
  `CLIENT_AGENT_TYPE` and the doc comments are what keep them apart; a future reader who conflates
  them again would couple the Agent type to the product name and split the fleet's package Sets
  without noticing.

- **Negative: the default `PRODUCT_NAME` equals the old parent directory.** `/opt/opamp-fleet` is
  today the level above the install root and afterwards is the install root. Harmless with nothing
  installed, and it makes the change smaller — but it means the path alone does not reveal which
  scheme produced it.

- **Negative: this ADR cannot be accepted before ADR-0082.** It supersedes three of that document's
  clauses, and a proposed ADR cannot supersede clauses of another proposed one in any binding
  sense. Accepting them out of order would leave the service's name decided by a clause that is
  itself not yet in force.

- **Carried from ADR-0053, positive: the `.rpm` produces a service that starts** on Fedora, RHEL
  and openSUSE Leap 16 / SLES 16 with SELinux enforcing — no new dependency, no policy module, no
  labelling step to keep alive, and self-update staging inherits the working label by construction.

- **Carried from ADR-0053, negative: the default install spans two directories** on the two
  platforms that split, so the manual and every path a support engineer greps for must name both.
  A `--root` install on an enforcing host still fails if the operator roots it somewhere
  unexecutable — a documented property of choosing a root, not a default anybody gets. A host
  mounting `/opt` `noexec` breaks, which is rarer than enforcing SELinux by orders of magnitude,
  and loud rather than silent.

- **Carried from ADR-0053, open: the coverage gap that hid the original bug is still open.**
  `crates/client/tests/service_smoke.rs` still excludes hosts with SELinux or AppArmor in the way,
  so nothing automated exercises the reason clause 3 exists. Superseding ADR-0053 moves that
  follow-up here; it does not discharge it.

- **Carried from ADR-0062, positive: the Client and everything its Supervisors spawn can drop
  root and `LocalSystem` on operator demand**, with both roots belonging to the account that uses
  them, and with no password anywhere in the Windows story.

- **Carried from ADR-0062, negative: the account is a trust boundary.** Whoever holds it can
  replace the binary in the layout, and the `PATH` symlink through `current` means an administrator
  invoking the CLI executes account-owned code — the manual must say so plainly. Managed Processes
  inherit the account, so anything needing ports below 1024 or root-only telemetry sources fails
  under it. That is the operator's informed choice, not this decision's.

- **Follow-ups, carried from ADR-0062:** the `.deb`/`.rpm` packaging grows a `postinst` account
  creation and a `--run-as` wiring; the MSI can offer the virtual account as a checkbox; the
  manual's `service install` section documents the flag, the ownership hand-over across both roots,
  and the trust boundary.

- **Follow-ups:** whether variant builds are ever actually published, and if so what the release
  pipeline's matrix looks like, is deliberately left open — this decision makes them possible
  without committing to shipping any. If they are, the `UpgradeCode` register needs a home, and the
  tension between artifacts named after the Set (ADR-0082 clause 3) and packages named after the
  product will need stating where ADR-0046 clause 4 requires all four artifacts of a target to share
  a name. The question of how an operator discovers what is installed on a host — the gap that made
  the instance name unrecoverable — is not answered here and would need its own decision.
