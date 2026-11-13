# Ticket: A privileged step reads a file one way: through no link, bounded and wiped

## What to build

The rule that a privileged step of the Client reads a file through no link, at a bound, into a
buffer that is wiped (ADR-0021 clause 11, ADR-0014) is written in five places, each holding a
different part of it:

- `nofollow::HeldDir::read` (`crates/fleet-agent/src/nofollow.rs`) refuses a link, but reads
  without a bound into a `Vec` that grows;
- `tls::read_opened` (`crates/fleet-agent/src/tls.rs`) has the 1 MiB bound and the buffer sized
  before the read;
- `enrol::read_privileged` (`crates/fleet-agent/src/service/enrol.rs`) walks the path one
  component at a time with its own `MAX_LINKS`, and `config_init` calls it, so an adapter calls into
  a composition root;
- `storage::handle::step_trusting` (`crates/fleet-agent/src/storage.rs`) walks again, with its own
  `MAX_HOPS` and its own trust rule;
- `tls::pkcs12_identity` and `config_init::read_password_file` each have a bound of their own and
  read by path.

Three fixes in the last count each closed one more read (6d8ce2c27, c9f6b2b5d, 7114c049a). Two
reads still break the rule:

- `config_file::read_held` reads `supervisor.toml` as root through `HeldDir::read`. The read has no
  bound, and the copies a growing buffer leaves behind are not wiped, although the file may hold
  `[packages] archive_key`. Its doc says the buffer is wiped.
- The questionnaire's `pkcs12_identity` (`config_init.rs`, the asked bootstrap file) reads by path
  and follows a link. Its default comes from `[tls] cert_file` in `supervisor.toml`, which the
  service's account may write. The reason `tests/privileged_steps_follow_no_link.rs` gives for
  allowing that read, "beside the program or in `--fleet-files`", no longer covers the path. The
  test allows a call per function, not per call site, so it cannot see the new caller.

Give `nofollow` one bounded read, sized before it reads and wiped, that every privileged read
takes, and move the component walk there. Route the PKCS#12 and password reads through it. Make
the allow-list of `privileged_steps_follow_no_link` name each call site with its reason, so a new
caller of an allowed function is a finding.

`docs/backlog/one-directory-handle-module.md` on the branch `docs/backlog-revision` proposes one
Unix module for work by directory handle. Whichever of the two is worked second takes the other up.

## Acceptance criteria

- [ ] Given a `supervisor.toml` larger than the bound, when a privileged step reads it, then the
  read is refused, naming the file and the bound.
- [ ] Given a link at the place of the bootstrap PKCS#12 file the questionnaire offers from
  `[tls] cert_file`, when an install runs as root, then the file is refused as a link and not read.
- [ ] Given a new call of an allowed by-path function in a privileged module, when
  `privileged_steps_follow_no_link` runs, then it fails, naming the call site.
- [ ] Given the Client's existing tests, when the change lands, then they pass, and no module of
  the role Adapter calls `service::enrol`.

## Read first

- Touches: `crates/fleet-agent/src/nofollow.rs`, `crates/fleet-agent/src/config_file.rs`,
  `crates/fleet-agent/src/config_init.rs`, `crates/fleet-agent/src/tls.rs`,
  `crates/fleet-agent/src/service/enrol.rs`, `crates/fleet-agent/src/storage.rs`,
  `crates/fleet-agent/tests/privileged_steps_follow_no_link.rs`
- ADR-0021 clause 11, ADR-0014, ADR-0044; `docs/ARCHITECTURE.md`, "Client supervision", the
  paragraph on `nofollow`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes what a root install reads, which the human judges.
