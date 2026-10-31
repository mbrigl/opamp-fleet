# Ticket: A privileged enrolment reads the `[tls]` files bounded, and not through the account's links

## What to build

`service config` enrols as root or an Administrator, and the enrolment reads the files `[tls]`
names (`ca_file`, `cert_file`, `key_file`) by path with an unbounded read. One of them, the CA copy
`service install` puts in the data root, lies in a directory the service's account owns, so a link
the account placed there is followed by the privileged step. A path the account wrote into
`supervisor.toml` can also name a device, which the read never ends on, or a file only root may
read.

A no-follow read of the CA copy alone does not close this: the account owns the bytes there and can
rewrite the `[tls]` paths and `endpoint` in `supervisor.toml`. Two ways are open, and which one is
the human's call, possibly with an ADR-0037 clause:

- the privileged enrolment reads each `[tls]` file only when it is a regular file, at a size cap,
  and through the held directory where it lies in the data root or the state directory;
- the privileged enrolment trusts the CA the run itself just copied, rather than re-reading the
  file the account may have changed.

## Acceptance criteria

- [ ] Given a link the account placed at the CA copy in the data root, when `service config` enrols,
  then the link is not followed.
- [ ] Given a `[tls]` path that names a device or a file larger than the cap, when a privileged step
  enrols, then it refuses the file, naming it, without reading it whole.
- [ ] Given the `tls.rs` `read` entry in `ALLOWED` of `privileged_steps_follow_no_link`, when the
  change lands, then its reason no longer names an open gap.

## Read first

- ADR-0037 clause 11, ADR-0019 clauses 25 and 26.
- `crates/fleet-agent/src/tls.rs` (`read`, `client_tls`), `config_init.rs` (`copy_ca_beside`,
  `read_certificates`), `service/enrol.rs`.
- `crates/fleet-agent/tests/privileged_steps_follow_no_link.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended. It changes a privileged step's
trust, and the choice between the two ways is a design decision.
