# A missed notification holds a restart requested during a delivery

**Kind:** defect.

**Observed.** A restart requested through another instance between the carrying instance's clear
of the restart it delivers and the delivery's end is held when its announcement arrives:
`take_up_restarts` reads the named Agents under their own locks. When the announcement is missed,
the instance takes up every Agent (`take_up_agents` and `take_up_restarts` with
`TakeUp::Everything`). That read runs without the Agents' locks, can find store and memory
agreeing between the clear and the delivery's end, and the delivery then takes the request out of
memory. It stays queued in the store, unsent until a later take-up reads it.

**Idea.** Read the held Agents' restarts under their locks after a missed notification too, in
one read, as the named take-up does; or have a delivery take its clear into memory only where no
take-up held a request since its clear.

**From.** The review of ticket adr-text-follows-the-code/01 (2026-11-12).
