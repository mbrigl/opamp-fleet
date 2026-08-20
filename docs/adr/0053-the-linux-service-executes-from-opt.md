# ADR-0053: The Linux service executes from `/opt` — a binary under `/var/lib` is one SELinux never lets systemd start

- **Status:** ⚪ superseded by [ADR-0084](0084-the-product-names-the-installation.md)
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

## Context

ADR-0010 roots the default system-scope install at the platform data directory —
`/var/lib/opamp-fleet/client/<instance>` on Linux — and that one root holds everything: the
`versions/` tree, the `current` pointer the service is registered against, the default `state/`
directory, and the default `client.toml`. ADR-0046 and ADR-0048 build the `.deb`/`.rpm` flow on
top of it: `%post` runs plain `service install`, and `/usr/bin/opamp-fleet-client` is a symlink
through that root's `current` pointer.

On the distributions the `.rpm` exists for, the service this registers can never start:

- Fedora and RHEL run SELinux with the targeted policy in **enforcing** mode by default, and
  openSUSE Leap 16 / SLES 16 have switched from AppArmor to exactly that (SELinux targeted,
  enforcing on a fresh install). This is no longer one distribution family's quirk but the
  default posture of the entire rpm world.
- Files created under `/var/lib` carry the SELinux type `var_lib_t`. systemd (`init_t`) may only
  `execve` types the policy marks as service entrypoints — `bin_t`, `usr_t`, and friends, through
  which a third-party service transitions into `unconfined_service_t`. `var_lib_t` is not such a
  type, for anybody: data directories are deliberately not executable by the init domain.
- The failure is deferred and silent at install time: the package installs, `service install`
  stages the layout and registers the unit, `systemctl enable` succeeds — and the first
  `systemctl start` dies with `status=203/EXEC` (Permission denied), an AVC denial in the audit
  log the unit's own journal never explains. The same binary runs fine from an interactive shell
  (`unconfined_t` may execute nearly anything), which makes the diagnosis actively misleading.
- The blind spot is on record: the service smoke test excludes "hosts with SELinux or AppArmor in
  the way" from coverage (`crates/client/tests/service_smoke.rs`), so no automated or scripted
  check ever met an enforcing host.

Two forces constrain the fix:

- **The layout is rewritten at runtime.** The self-update (ADR-0020) stages new version
  directories while the service runs, so any fix must hold for files the *daemon* creates later,
  not only for what the package laid down once. A one-time `chcon` dies at the next staging or
  the next filesystem relabel; a persistent file-context rule needs `semanage` from
  `policycoreutils-python-utils`, which is not guaranteed to be installed and would become a
  runtime dependency of an application that otherwise needs none.
- **The unit's data paths are load-bearing on managed hosts.** The instance identity, the
  credential, and `client.toml` live under the `/var/lib` root on every host already in the
  fleet. A fix that moves *them* forces a fleet-wide state migration inside a package upgrade —
  a copy across a possible filesystem boundary, torn if the upgrade is interrupted, on hosts
  whose whole point is to be managed unattended.

## Decision

We will split the **Linux system-scope defaults**: the executable layout — `versions/` and the
`current` pointer — moves to **`/opt/opamp-fleet/client/<instance>/`**, while the instance's data
— `client.toml` and `state/` — stays at **`/var/lib/opamp-fleet/client/<instance>/`**.

- **Why `/opt` starts.** Its default file context is `usr_t`, an entrypoint type through which
  systemd transitions a third-party service into `unconfined_service_t` — the mechanism the
  targeted policy provides precisely so vendor software outside the distribution's packages can
  run enforcing. Files the self-update stages later inherit the directory's label, so staging
  keeps working with no SELinux tooling, no policy module, and no new dependency. FHS 3.0
  sanctions the shape: `/opt` is for add-on application software, and an add-on package's
  variable data belongs under `/var` — the split is the standard-conformant layout, not a
  compromise. It is also the field-proven one: Elastic Agent, whose versioned-directory scheme
  ADR-0010 adopted, installs to `/opt/Elastic/Agent` on Linux.
- **Only the defaults split, and only here.** `--root` keeps its meaning unchanged — everything
  under the one directory the operator names, whose labeling is then the operator's business
  (documented in the manual). macOS, Windows, and the Linux user scope are untouched: no
  enforcing policy stops them, and a `systemd --user` service runs in the user's own domain.
- **The upgrade on a managed host is a re-registration, not a migration.** The unit's `--config`
  and `--state-dir` arguments keep their `/var/lib` paths; only `ExecStart` changes. `%post`'s
  `service install` stages into `/opt`, rewrites the unit, and the restart the scriptlet already
  performs picks it up — identity, credential, and state never move.
- **The packaging follows** (amending ADR-0048's table, whose mechanism stands unchanged): the
  `postinst`/`%posttrans` symlink targets
  `/opt/opamp-fleet/client/default/current/opamp-fleet-client`; `postinst` additionally removes
  the now-orphaned `versions/` and `current` under the old `/var/lib` default root — binaries
  only, one-time, after `service install` has succeeded and nothing references them. `postrm` on
  **remove** deletes the `/opt` instance tree (it holds nothing but staged binaries) and on
  **purge** additionally the `/var/lib` instance directory, exactly ADR-0048's remove/purge
  distinction with the paths redrawn.

## Alternatives considered

- **A persistent SELinux file context from the scriptlets** (`semanage fcontext -a -t bin_t …` +
  `restorecon -R`) — needs `policycoreutils-python-utils`, which the rpm would have to require or
  guard; a guarded fallback fails silently, which is this bug with extra steps. Label management
  in shell, re-run after every self-update and relabel, to keep executing from a directory the
  policy says should not be executed from.
- **Ship an SELinux policy module in the `.rpm`** — the most packaging-orthodox answer and the
  heaviest: a policy to author, build, and verify across Fedora, RHEL, and the newly-enforcing
  SUSE family, for a Client whose need is fully met by standing in the right directory. Remains
  the natural follow-up if a confined domain is ever wanted (CIS Server Level 2 flags
  `unconfined_service_t` daemons); nothing here forecloses it.
- **`chcon` at staging time** — not persistent across an autorelabel (`/.autorelabel`,
  `restorecon`), and it puts SELinux-specific tooling into the Client's own runtime path on every
  distribution, enforcing or not.
- **Move the whole root — state included — to `/opt`, Elastic-style** — one constant instead of
  two defaults, but it forces the fleet-wide state migration the Context rules out, and parks
  variable data in `/opt` against FHS. Rejected: the split costs one extra default path and
  costs the fleet nothing.
- **The executable layout under `/usr/lib` or `/usr/libexec`** — the labels would work, but the
  layout is application-owned and rewritten at runtime by the self-update, and ADR-0046/0048
  drew the ownership line exactly there: the package manager's hierarchy is the package
  manager's. An application mutating `/usr` at runtime blurs what those ADRs sharpened; `/opt`
  is the FHS home for software a distribution's package manager does not own.

## Sources / Prior art

- systemd `status=203/EXEC` under SELinux — the failure signature, and why the same binary runs
  interactively (`unconfined_t`) but not as a service:
  <https://thomaspowell.com/2026/04/03/the-selinux-203-exec-systemd/>; a real-world case of a
  service binary in a data directory failing exactly this way (GitHub Actions runner):
  <https://github.com/actions/runner/issues/1606>.
- `unconfined_service_t` — the targeted policy's mechanism for third-party services started by
  init from entrypoint-typed files (Dan Walsh, its author):
  <https://danwalsh.livejournal.com/70577.html>; Red Hat's documentation of unconfined process
  domains:
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/selinux_users_and_administrators_guide/sect-security-enhanced_linux-targeted_policy-unconfined_processes>;
  CIS Server Level 2 flagging unconfined daemons (why a policy module stays a possible follow-up):
  <https://access.redhat.com/solutions/6714611>.
- openSUSE Leap 16.0 / SLES 16 release notes — SELinux targeted policy, enforcing by default,
  replacing AppArmor:
  <https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/html/release-notes-leap-160/index.html>.
- FHS 3.0 — `/opt`: add-on application software packages (§3.13), with variable data placed
  under `/var`: <https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch03s13.html>.
- Elastic Agent installs to `/opt/Elastic/Agent` on Linux (`--base-path` to override) — the
  same layout lineage ADR-0010 cites, standing where this ADR moves to:
  <https://www.elastic.co/docs/reference/fleet/installation-layout>.
- ADR-0010 (the layout and the default root this amends), ADR-0020 (the self-update that
  rewrites the layout at runtime), ADR-0046 (the native packages), ADR-0048 (the `PATH` symlink
  and remove/purge cleanup whose paths this redraws).

## Consequences

- Positive: the `.rpm` produces a service that starts on Fedora, RHEL, and openSUSE Leap 16 /
  SLES 16 with SELinux enforcing — out of the box, with no new dependency, no policy module, and
  no labeling step to keep alive. Self-update staging inherits the working label by
  construction.
- Positive: hosts already in the fleet upgrade seamlessly — the unit's data paths do not change,
  so identity, credential, configuration, and state stay exactly where they are; only
  `ExecStart` and the `PATH` symlink move.
- Negative / trade-offs: the default install spans two directories instead of one — the manual
  and every path a support engineer greps for must name both. A `--root` install on an enforcing
  host still fails if the operator roots it somewhere unexecutable; that is now a documented
  property of choosing a root, not a default anybody gets. A host mounting `/opt` `noexec`
  breaks — rarer than enforcing SELinux by orders of magnitude, and loud rather than silent.
- Negative: upgraded hosts keep an orphaned `versions/` + `current` under `/var/lib` until the
  first packaged upgrade's `postinst` cleans them; manual (`.7z`) installs at the old default
  keep theirs until reinstalled, which is their operators' call (ADR-0010: manual layouts are
  the operator's).
- Follow-ups: the manual's path table and the README smoke checklist gain the split paths and an
  SELinux-enforcing step; a confined SELinux policy module remains an option for CIS-L2
  environments, by topic only.
