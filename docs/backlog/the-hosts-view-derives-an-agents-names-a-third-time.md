# The Hosts view derives an Agent's names a third time

**Kind:** doubt, structure.

**Observed.** Since ticket 01 of `revision-2026-11-18`, `listing::display_values` in
`crates/fleet-server/src/listing.rs` is the one place that derives an Agent's display values. It
takes the service name from the identifying attributes only and the instance name from the
non-identifying ones only. `hosts_of` in `crates/fleet-server/src/fleet/views.rs` (near lines 37
to 57) derives the service name and the instance name of each `HostAgentView` itself, and looks
each up in the identifying attributes first and then in the non-identifying ones. So an Agent
that reports `service.instance.name` as identifying shows an instance name in the Hosts view and
none in the list.

**Idea.** Under the CONVENTIONS entry "A rule two paths apply is one function", `hosts_of` takes
the names from `listing::display_values`, after the human decides which lookup is right. A test
holds an Agent's names in the Hosts view against its listed record.

**From.** The review of `plan/revision-2026-11-18-frontier` at 6d7b2732f (2026-11-18).
