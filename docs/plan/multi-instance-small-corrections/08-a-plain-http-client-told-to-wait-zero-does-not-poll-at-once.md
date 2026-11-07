# Ticket: A plain-HTTP Client told to wait zero does not poll again at once

## What to build

The `opamp` plain-HTTP driver (`client/http.rs`) sleeps exactly the wait a Server names and then
polls again. It does so after an `Unavailable` reply (`on_reply` answers the reply's `RetryInfo`)
and after a `503` or `429` (`Retry-After`, `MIN_RETRY_INTERVAL` only when the header is missing or
cannot be read). A wait of zero, which a Server sends when it leaves `retry_after_nanoseconds`
unset or answers `Retry-After: 0`, makes the Client poll again at once, as fast as the Server
answers, for as long as it keeps answering so. This Server always names 30 s (1 s for its
`busy()` answer), so only a third-party Server reaches it.

The WebSocket driver had the same hole after ticket 37 of several-server-instances and closed it
there: it waits the named wait or the next backoff step, whichever is longer, and `Unavailable`s in
a row grow along the backoff until a connection handles a reply. The plain-HTTP driver predates
that change and has no backoff on this path at all.

A plain-HTTP Client told to wait zero, or less than a backoff step, waits at least the step, and
consecutive refusals grow along the backoff of ADR-0012 clause 4 until an exchange is answered.

## Acceptance criteria

- [ ] Given a Server that answers every poll `Unavailable` with no wait, when the plain-HTTP
  driver runs, then its polls come at growing intervals, not at once
- [ ] Given the same with `503` and `Retry-After: 0`, when it runs, then likewise
- [ ] Given a Server that names a wait longer than the step, when the driver honours it, then it
  waits the named wait and not more

## Read first

- ADR-0012 clause 4 and its *Context* on the Client honouring `Unavailable`
- `crates/opamp/src/client/http.rs` `run`, `exchange`, `retry_after`; `client/backoff.rs`;
  `client/ws.rs` `run_from` and `next_delay`, which do this for the WebSocket

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
