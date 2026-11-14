# The renewal test races the reconnect its certificate asks for

**Kind:** defect, a flaky test.

**Observed.** `a_client_renews_each_certificate_before_it_expires`
(`crates/fleet-agent/tests/certificate_renewal_e2e.rs`) failed once under `scripts/check-all.sh` on
2026-11-14, with "the Client stayed with the Server". It passed in the run before, on the same
code, and in five runs of that test alone. The test stops polling once it has read the third
certificate from the Client's state directory. Then it asserts that the Server holds the Agent as
connected. The Client stores a renewed certificate with the offered connection settings and then
reconnects with it (`OfferOutcome::Reconnect` in `crates/fleet-agent/src/transport/mod.rs`). The
assertion can land between that store and the new connection.

**Idea.** The test waits, within a bound, until the Agent is connected again after the third
certificate, instead of reading the snapshot once.

**From.** The implementation of revision-2026-11-12/11 on its ticket branch (2026-11-14).
