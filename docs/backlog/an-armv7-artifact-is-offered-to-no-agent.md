# An `armv7` artifact is offered to no Agent

**Kind:** defect, accepted for now.

**Observed.** `opamp-fleetctl package fetch` folds an asset's platform through
`fleet_core::platform` and passes a token the table does not know through
(ADR-0039
clause 6). A Collector release names its 32-bit ARM build `linux_armv7`, so the tool
lists `linux/armv7`, and the Server stores the entry under that name. No Agent reports `armv7`:
Go's `GOARCH` and Rust's `std::env::consts::ARCH` both say `arm`. The entry is uploaded and
never offered. Before the tool read the core's table, it passed such an asset over.

**Idea.** The table of
ADR-0039
clause 6 folds `armv7` onto the token an Agent reports. That
changes the table, so it needs an ADR superseding
ADR-0039.
The asset names of current and older
Collector releases were not checked against the network.

**From.** The review of `fix/package-fetch-platforms` (2026-11-15).
