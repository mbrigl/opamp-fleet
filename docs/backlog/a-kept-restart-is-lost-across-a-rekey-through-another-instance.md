# A kept restart is lost across a re-key through another instance

**Kind:** defect, accepted for now.

**Observed.** On PostgreSQL, a restart command whose send failed and whose put-back the store
refused is kept for retry by the instance that sent it, keyed by the Agent's `instance_uid`
(`put_back`, `retry_restarts` in `crates/fleet-server/src/fleet/held.rs`). When another instance
re-keys the Agent before the retry, the retry finds the row gone, logs a warning naming the Agent
and drops the put-back: the Operator's restart is lost. ADR-0025 clause 8 names this as a case not
seen, and the manual's restart route says so.

The keeping instance cannot learn the new identity. `rekey` in `postgres/agents.rs` copies the row
under the new key, deletes the old one and announces only the old identity; no column, table or
notice names the new one. The read back of a re-key covers only one this instance made itself.

**Idea.** One of: the re-key writes `(old, new)` to a table a put-back reads when its row is gone (a
migration, and a rule to prune it); a notice that names both identities (wider than clause 9
allows, and still lost with a missed notice); the put-back as a column of the Agent's row that the
re-key carries (a migration). The human chose to keep the case not seen for now (2026-11-12).

**From.** Ticket revision-2026-11-06/07, stopped before any code (2026-11-12).
