# Ticket: The fleet lists page and name the holder

## What to build

The four lists that grow with the fleet page by cursor in a `Link` header, in a stable order with a
unique tiebreak, filtered by the Server before the page is cut; a request without `limit` keeps the
whole array up to a bound. The filters run on derived columns written with each record; inherited
labels and Host values are joined at read time. An Agent's view carries its holder, the instance id
and host name its lease records, empty without a live lease; the UI shows it in the Agent's detail
and the list filters by it. On the filesystem store the same queries are answered, except `holder`,
which is answered `400`. The port's `query` takes the filter on the Host an Agent arrived over,
and an instance reads through it the Client Agent of an Agent behind a Gateway that another
instance carries, so that the Agent inherits its labels in memory too; a Gateway's artifact
download reads through it the Agents behind that Gateway rather than the whole fleet.

## Acceptance criteria

- [ ] Given 2 500 Agents, when the list is read with `limit=1000`, then three pages come back with a
  `Link` to the next, no Agent twice and none missing
- [ ] Given a filter on a label an Agent inherits from its Client Agent, when the list is read, then
  the Agent is in it
- [ ] Given an Agent carried by A, when it is read, then its view names A's instance id and host,
  and `holder=<A>` lists it and `holder=none` does not
- [ ] Given the filesystem store, when the list is read with `holder`, then the answer is `400`
- [ ] Given a request without `limit`, when the list is read, then the whole array comes back up to
  the bound, as today
- [ ] Given an Agent behind a Gateway carried by B and its Client Agent carried by A, when B places
  the Agent, then it inherits the Client Agent's labels

## Read first

- ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server
  clauses 1 to 8;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  13; ADR-draft-the-servers-state-and-its-stores clause 21
- ADR-0025 for the REST API's shapes
- Golden path: the REST API integration tests of the agents list
- Seam: the REST API on both stores

## Blocked by

- docs/plan/several-server-instances/04-the-fleet-record-is-written-per-column.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. Its scope is isolated and a test decides it, but the harness allows no
unattended work.
