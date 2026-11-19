# The contract pair is dropped

**Kind:** schema, release.

**Observed.** Release 0.8.5 is release X of the contract pair, the Deployments' mirror columns
`packages` and `configurations` and the Hosts' `instance_uids` arrays. Its migration 19 gives each
column a default and names 0.8.4 as the oldest Server, and no statement it runs while serving
names them. A Server of 0.8.4 still names them in its `INSERT`s, takes a released Agent out of an
array with `array_remove`, empties an array on the mark of a Gateway and filters it in the upsert.

**Idea.** Release X+1, the release after 0.8.5, drops the columns by a migration, together with the
comments that keep them. The migration names 0.8.5 as the oldest Server
([SRV-state-stores-contract-pair §22](../adr/SRV-state-stores-contract-pair.md)).
It cannot come earlier, since a Server of 0.8.4 beside it still names the columns. The test SQL that
names them to simulate an older release goes or changes with them, and the tests that drop the
columns to show 0.8.5 survives the drop
(`the_mirror_columns_are_written_no_more`, `the_hosts_arrays_are_named_no_more`) become plain
tests of the schema it leaves. The check that no served statement names a column of the pair
(`no_statement_names_a_column_of_the_contract_pair` in `crates/fleet-server/src/postgres/schema.rs`)
goes too, with its reader of served code (`served_literals`, `raw_string_at`,
`names_the_contract_pair`), and the ADR's Enforcement stops citing it. The
same Enforcement still introduces its tests as carrying `Verifies:` "once it has one" of a
number; that wording goes with it.

**From.** The triage of the plan by the human (2026-11-15); replaces ticket 02 of
`the-contract-pair-is-removed`. Restated when release 0.8.5 took release X (2026-11-19). The
design revision of 2026-11-19 named the check and its reader.
