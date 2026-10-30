# Artifact documents

[← User Manual](../manual/README.md) · [The Client](../manual/client.md) ·
[Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how a wrapped agent is packed, and whoever changes what its kind
knows. Every wrapped kind has a document here, `<agent>.md`, the one place both sides state the
same facts about its artifact, so a release that moves something can be answered on both at once
([ADR-0017](../adr/0017-supervisor-mode-and-its-kinds.md) clause 20).

Each document has the same eight sections: the source (repository, tag form, version reading); the
assets per platform and their naming; integrity (checksum form and location); the treatment (as
published, or which repack); the shape in the delivered tree; what the Client derives from it, per
platform, and why; the Configurations the tool uploads and what the agent reads them as; and what
can change upstream, with the test that goes red.

Each is pinned by one test per side: one against the plan `opamp-fleetctl package fetch` makes in
`crates/fleet-tools/src/fetch.rs`, one against the kind's constants in
`crates/fleet-agent/src/supervisor/`, and both name the document. `collector` and `command` have
none: what they run is installed as published or named by its block, so there is no second end to
keep in step.
