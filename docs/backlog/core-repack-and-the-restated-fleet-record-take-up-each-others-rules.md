# `feat/core-repack` and the restated record take up each other's rules

**Kind:** doubt, coordination.

**Observed.** `feat/core-repack` carries the `supervisor.*` label rule of the fleet record's
clause 20 (ADR-0030) and the operator program's kept-link rule. The ADRs of several Server
instances, ADR-0040 to ADR-0043 and ADR-0045 among them, were written
without them. The feature spec of several Server instances said that whichever of the two reaches
`main` second takes the other's rule into its restatement; with the spec closed, nothing else in
the tree says so.

**Idea.** When `feat/core-repack` is taken up again, check its restated ADRs against those accepted
since it branched against its rules before it reaches `main`.

**From.** The *How the remaining work runs* of `docs/plan/several-server-instances/spec.md`, closed
by the design revision of 2026-11-12.
