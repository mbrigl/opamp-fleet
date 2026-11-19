# Ticket: The release-workflow test reads the workflow as YAML

## What to build

`crates/fleet-tools/tests/release_workflow.rs` slices the workflow by string (`job()`, near line
225) and checks permissions by substring. The identifier migration put the word `attestations`
into a comment of the image job, and the test failed (fixed in a0dc7cd17). The fix still matches
substrings: a key in a comment, or a step moved under another job, is read wrong.

The test parses `.github/workflows/release.yml` and asserts on the jobs, their `permissions`
and their steps' `uses` as values.

## Acceptance criteria

- [ ] Given a comment in the image job that names `attestations: write` or the attesting action,
  when the test runs, then it passes, decided by a self-test on such a fixture.
- [ ] Given an image job that is granted `attestations: write`, or that has a step using the
  attesting action, when the test runs, then it fails and names the job.
- [ ] Given the change, when the tests of `fleet-tools` run, then every assertion they made
  before is still made, unchanged in number and name.

## Read first

- Touches: `crates/fleet-tools/tests/release_workflow.rs`, `crates/fleet-tools/Cargo.toml`
- OPS-releases-attestations-server-image §11; OPS-actions-pinned-by-sha.
- A YAML parser as a dev-dependency is a new crate in the supply chain (`deny.toml`); find out
  first whether one is in `Cargo.lock` already.

## Blocked by

- None (can start immediately)

## Mode

with the human: DEV-maintainer-decides-agents-implement §5, and a new dev-dependency is the
human's to accept.
