# A verdict on an older hash replaces the status of the one applying

**Kind:** defect.

**Observed.** A Supervisor's Agent offered hash H2 while H1 is still applying hands H2 to the
process adapter behind H1 and reports `APPLYING` H2. H1's verdict then arrives, and
`config_applied` sets the status to `APPLIED` H1 unconditionally
(`crates/fleet-agent/src/supervisor/agent/remote_config.rs`). The Agent reports H1 as its last
configuration while H2 waits in the queue. The Server, seeing H1 where it wants H2, answers the next
message with the offer of H2 again. The status the Agent holds is now `APPLIED` H1, so clause 56
of ADR-0046 does not take the
offer of H2 as a repeat: H2 is handed over a second time and the Managed Process restarts once
more. It takes two rollouts within one apply grace.

**Idea.** A verdict on a hash other than the one the status holds as `APPLYING` leaves the status
as it is; or the Agent remembers the hashes handed over and not yet decided, and an offer of one of
them is a report.

**From.** Ticket a-repeated-offer-is-applied-once/01 (2026-11-12).
