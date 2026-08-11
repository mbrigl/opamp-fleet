# ADR-0048: The packaged CLI on `PATH` is a symlink through `current`, and a package removal takes every staged version with it

- **Status:** 🟡 proposed
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

ADR-0046 clause 2 has the `.deb` and `.rpm` deliver the release binary to
`/usr/bin/opamp-fleet-client`, and `service install` stage a *copy* into the versioned layout
(ADR-0010) that the service is registered against — `<root>/current/opamp-fleet-client`, never a
version directory directly. The two ownerships staying disjoint is what keeps `dpkg -V` quiet and
keeps an `apt upgrade` from silently reverting a fleet decision, and ADR-0046's Consequences record
the cost: after a self-update, the delivered binary and the running binary drift apart.

A support incident showed where that cost actually lands. A host was fleet-updated 0.2.0 → 0.2.1,
then had its package manually reinstalled at 0.2.0. Every question the operator asked the host —
`dpkg -l`, and crucially `opamp-fleet-client --version` — answered 0.2.0, while the service ran what
`current` named. The manual says "`opamp-fleet-client --version` and the fleet view are the truth"
([`client.md`](../manual/client.md)), but that sentence is only true when the *service's* binary
answers — and the shell resolves `opamp-fleet-client` through `PATH` to the package-delivered file,
which is precisely the binary that drifts. The operator's first diagnostic command is the one place
the drift is guaranteed to mislead.

The install layout already has the answer for the service: nothing invokes a version binary
directly, everything goes through the `current` pointer, so a version switch never re-registers
anything. The CLI on `PATH` is the one remaining entry point that violates that rule.

## Decision

We will make the CLI on `PATH` resolve through the layout's `current` pointer, never to a delivered
version binary: the `.deb` and `.rpm` deliver the payload to **`/usr/libexec/opamp-fleet-client`**
(off `PATH`; FHS 3.0 sanctions `/usr/libexec` for internal binaries), and
**`/usr/bin/opamp-fleet-client` becomes a symlink** to the default system layout's
`current` binary — `/var/lib/opamp-fleet/client/default/current/opamp-fleet-client` — maintained by
the maintainer scriptlets:

| hook | runs | does |
|---|---|---|
| `postinst` / `%post` | after files land | `/usr/libexec/… service install`, then `ln -sfn` the symlink |
| `%posttrans` (rpm only) | end of the transaction | `ln -sfn` again — see below |
| `postrm` / `%postun` | after a real removal (never an upgrade) | remove the symlink (only if it is one), `versions/` and `current`; on dpkg **purge**, the instance directory itself |

`%posttrans` exists because rpm's upgrade ordering erases the *old* package's files (the regular
file the previous release delivered at `/usr/bin/opamp-fleet-client`) **after** the new package's
`%post` has run — deleting the symlink `%post` just created. The transaction scriptlet runs after
that erasure, so the link it lays is the one that survives. On dpkg the obsolete file is removed
during unpack, before `postinst configure`, so no equivalent is needed.

Two consequences inside the Client itself:

- **Staging becomes rewrite-free when the bytes are identical.** `service install` invoked through
  the symlink *runs from* `<root>/versions/…/opamp-fleet-client`; re-staging would write over the
  very file it executes, which Linux refuses (`ETXTBSY`). `stage_current_exe` now compares hashes
  and skips the write when the staged binary already holds the running bytes — an idempotent
  re-install stays idempotent, and the documented post-install step
  (`opamp-fleet-client service install --endpoint …`) keeps working when it arrives through the
  link.
- The maintainer scripts call the payload at its `/usr/libexec` path, never the symlink: it is the
  file the package guarantees to exist at that moment.

**A real package removal also uninstalls every staged version.** `service uninstall` itself keeps
deleting nothing — ADR-0010's rule protects the manual flows, where the layout is the operator's.
But the support incident's second half was a removal that only *looked* complete: the package went,
the layout stayed, and the next install came up on the surviving `current` pointer running a version
the operator believed uninstalled. So the package's `postrm` finishes what a removal means on a
package-managed host:

- **remove** deletes `versions/` and the `current` pointer. The state directory and `client.toml`
  stay — an instance identity and a credential the operator typed are not binaries, and a reinstall
  picks them back up (a stale `installed-package.json` is already discarded at startup when it does
  not name the running release).
- **purge** (dpkg only; rpm has no equivalent) deletes the instance directory whole — state, logs
  and configuration included. Purge is dpkg's word for "leave nothing".

The packaged symlink and the removal both target the **default system root and default instance** —
the only root a packaged install ever uses, because `postinst` calls plain `service install`. An
operator who chooses `--root` or `--instance` is doing a manual install (`.7z`), where no package
writes to `/usr/bin` or deletes a layout at all.

This amends ADR-0046 clause 2's delivery table for the Linux packages; everything else there —
one delivered file, `service install` as the only install path, no shipped unit — stands.

## Alternatives considered

- **Ship the symlink as a packaged file instead of creating it in scriptlets.** `cargo-deb` can
  (asset tables with `preserve-symlinks`); `cargo-generate-rpm` does not document symlink assets at
  all. Two tools, two mechanisms, one of them undocumented — the scriptlets are one mechanism that
  both formats honour, and the removal guard (`only if it is a symlink`) keeps them polite.
- **A launcher shim at `/usr/bin` that execs `current`.** A second code path with its own failure
  modes (root discovery, exec error reporting), permanently delivered, to do what one symlink does.
- **Teach `service install` to write `/usr/bin` itself.** It knows the root, but a user-scope or
  custom-root install must not touch `/usr/bin`, and writing into the package manager's directory
  from the application crosses exactly the ownership boundary ADR-0046 drew. The scriptlets are the
  package's side of the line; the layout stays the Client's.
- **Documentation only.** The manual already carried the warning, and the incident happened anyway.
  The first diagnostic command has to answer for the running service, not for a footnote.

## Sources / Prior art

- [cargo-deb](https://github.com/kornelski/cargo-deb) — maintainer-scripts pickup
  (`preinst`/`postinst`/`prerm`/`postrm`), symlink asset support.
- [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm) — scriptlet options
  (`post_install_script`, `post_uninstall_script`, `post_trans_script`); symlink assets
  undocumented.
- [rpm scriptlet ordering](https://docs.fedoraproject.org/en-US/packaging-guidelines/Scriptlets/) —
  new `%post` runs before the old package's files are erased; `%posttrans` runs last.
- [FHS 3.0 `/usr/libexec`](https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch04s07.html) —
  binaries run by other programs rather than by users, off `PATH`.
- The `alternatives`-style indirection every Debian-managed toolchain uses: what users invoke on
  `PATH` is a link that is repointed, never the binary that moves.

## Consequences

- Positive: `opamp-fleet-client --version` — and every other CLI invocation on `PATH` — answers for
  the binary the service actually runs, in both drift directions (fleet updated the host, or an
  operator hand-reinstalled an older package). The manual's "`--version` is the truth" sentence
  becomes unconditionally true on Linux.
- Positive: the ownership boundary sharpens rather than blurs — the package owns a payload off
  `PATH` and a constant link; the layout owns everything the link resolves to; `dpkg -V` and
  `rpm -V` stay quiet through every fleet update.
- Positive: a package removal followed by an install of an older release comes up on that older
  release — no surviving `current` pointer outranking the operator's decision, which is the drift
  that opened this ADR.
- Negative / trade-offs: `dpkg -l` still names the delivered version, not the running one — that
  is inherent to a package manager and stays documented. Until `service install` has run once
  (which `postinst` does), the symlink dangles; a broken-by-hand layout makes the CLI fail loudly
  rather than answer for the wrong binary, which is the better failure.
- Negative: `apt remove && apt install` no longer preserves the running version across the gap —
  the reinstalled package's own binary is what comes up, staged fresh. That is the point, but it
  is a behaviour change from ADR-0046's "deletes nothing else", which this amends.
- Negative: the symlink assumes the default system root and instance; a packaged install combined
  with a hand-moved root leaves a dangling link. That combination is already unsupported — the
  packages know only the default install.
- Follow-ups: Windows has the same drift (`INSTALLFOLDER\opamp-fleet-client.exe` is the delivered
  binary and nothing on `PATH` goes through `current`); whether the MSI should lay a junction-based
  equivalent is its own decision. macOS remains a manual install and is untouched.
