# Ticket: A Client Agent bound elsewhere is inherited at once

## What to build

On PostgreSQL, an instance places the Agents it carries under their Client Agent in memory, and
reads the Client Agent of a Host from the database when another instance carries it
(`client_agents_elsewhere` in `fleet.rs`). It reads when it places the group. When the host
register binds a Client Agent to its Host on one instance just before another instance first places
a fellow Agent of that Host, the second instance may place the group before the binding reaches
its copy of the register. It then places nothing under the Client Agent, and the fellow Agent
inherits its labels in that instance's memory only once the group is placed again: when the Client
Agent's labels change, an Agent of the group is forgotten, the Agent's report changes, or the
instance has missed a notification and reads again. The fleet view, the reach counts and what a
rollout releases are read from the database and are not affected. The manual says so under
"Labels on several instances".

Behind a Gateway the register binds no Agent, and an instance finds the group's Client Agent by
reading the Agents that arrived over the Gateway's Host. It places such a group again when the
labels of the Client Agent it placed the group under change. A Client Agent that first arrives
behind the Gateway on another instance after the group was placed is not found that way: the
fellow Agent inherits its labels in this instance's memory only once the group is placed again,
when an Agent of the group joins, leaves or changes its report here, or the instance has missed a
notification and reads again. No notification today says that an Agent arrived over a Gateway's
Host on another instance.

A notification that changes the bindings of a Host places that Host's group again, and one that
announces a Client Agent arriving behind a Gateway places that Gateway's groups again, so the
fellow Agent inherits at once in both cases, and the manual's section goes. This does not block
G-20: the fleet view, the reach counts and what a rollout releases read the database.

## Acceptance criteria

- [ ] Given a Client Agent that instance A binds to its Host, and a fellow Agent of that Host that
  instance B placed before the binding reached B, when the binding's notification reaches B, then
  the fellow Agent inherits the Client Agent's labels in B's memory
- [ ] Given a fellow Agent behind a Gateway that instance B placed, and its Client Agent arriving
  behind the same Gateway on instance A afterwards, when A has taken it, then the fellow Agent
  inherits the Client Agent's labels in B's memory
- [ ] Given the manual, when it is read, then "Labels on several instances" is gone

## Read first

- ADR-0025
  clauses 8 and 9; ADR-0030 clauses 18 and 19
- `fleet.rs` (`place`, `inherit_held`, `client_agents_elsewhere`), `revocation.rs` (how a register
  notification is read again)
- The manual's "Labels on several instances"

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
