# The contract pair is dropped

**Kind:** schema, release.

**Observed.** Release 0.8.5 is release X of the contract pair, the Deployments' mirror columns
`packages` and `configurations` and the Hosts' `instance_uids` arrays. Its migration 19 gives each
column a default and names 0.8.4 as the oldest Server, and no statement it runs while serving
names them. A Server of 0.8.4 still names them in its `INSERT`s, takes a released Agent out of an
array with `array_remove`, empties an array on the mark of a Gateway and filters it in the upsert.

**Idea.** Release X+1, the release after 0.8.5, drops the columns by a migration, together with the
comments that keep them. The migration names 0.8.5 as the oldest Server (clause 22 of the draft
that supersedes SRV-state-stores-contract-pair,
`docs/adr/SRV-state-stores-contract-pair.md`).
It cannot come earlier, since a Server of 0.8.4 beside it still names the columns. The test SQL that
names them to simulate an older release goes or changes with them, and the tests that drop the
columns to show 0.8.5 survives the drop
(`the_mirror_columns_are_written_no_more`, `the_hosts_arrays_are_named_no_more`) become plain
tests of the schema it leaves.

**From.** The triage of the plan by the human (2026-11-15); replaces ticket 02 of
`the-contract-pair-is-removed`. Restated when release 0.8.5 took release X (2026-11-19).
