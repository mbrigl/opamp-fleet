# ADR-0031: The lists that grow with the fleet are paged by cursor and filtered on the Server

- **Status:** 🟢 accepted
- **Date:** 2026-11-07
- **Deciders:** Markus Brigl
- **Applies to:** the list routes `GET /api/v1/agents`, `GET /api/v1/hosts`, `GET /api/v1/certificates` and `GET /api/v1/revocations` in `crates/fleet-server/src/api.rs` and the OpenAPI document generated from them, the queries the fleet and the revocation register answer for them in `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/revocation.rs` and every store adapter behind them, the Agents and Hosts tabs of the bundled UI in `crates/fleet-server/static/index.html`, and the reads `query_page` and `reported_values` of the `AgentStore` port in `crates/fleet-server/src/agent_store.rs` and its PostgreSQL adapter with the columns it reads, and every document or script that reads one of these lists

## Context

`GET /api/v1/agents` answers the Agents the Server knows as a JSON array of `AgentView`s
([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 1). The bundled UI shows them in
a table it refreshes every two seconds: one text field matches a substring across names, status,
OS, platform, versions, attributes, labels, capabilities and package statuses, and the column
headers sort by name, connection, health, configuration status, version, OS, sequence number and
last seen. The Deployment form's attribute suggestions and the Configuration form's type
suggestions read the same fleet. `GET /api/v1/hosts`, `GET /api/v1/certificates` and
`GET /api/v1/revocations` answer arrays as well, polled every five seconds by the Hosts tab. Each
of the four grows with the fleet: one entry per Agent, per host, per certificate issued and not yet
expired, per revocation. Answered whole, and filtered, sorted and counted in the browser, each
response would grow with it.

The other lists do not. Configurations, Deployments, Packages and bootstrap certificates are made
by an operator, one at a time, and stay few. The enrolment queue holds at most 1024 requests
([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).

A fleet is up to about 100 000 Agents (the specification's Assumptions), held in PostgreSQL and
served by several Server instances. At that size a whole array is tens of megabytes every
two seconds per open browser, and an instance that does not hold every record in memory would have
to read all of them for each request. An offset into a list that changes while it is read skips or
repeats entries; a cursor that names the last entry read does not, and costs the same on every
page.

The REST API is the fleet's stable contract (G-5), versioned by path
([ADR-0032](0032-configurations-and-the-rest-api.md) clause 7). Scripts and external UIs read these
arrays. Whatever pages them must not change what such a client receives while the list is
small enough to send whole, and must not cut a list short without the client noticing.

This ADR adds to [ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) and changes none of its
clauses: its clause 1 says what an Agent's entry is and not how many a response holds, and its port
gains two reads beside those of its clause 2 (clause 9 here).

## Decision

We will let the four lists that grow with the fleet be read a page at a time, by an opaque cursor
over a stable order, with filters the Server applies, while a request that asks for no page is
answered with the whole array up to a bound.

1. **A page holds the entries the whole list holds, in the same shape.** `GET /api/v1/agents`
   answers the records as `AgentView`s, as [ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md)
   clause 1 decides, and the other three lists answer the entries they answer whole. Paging changes
   how many entries a response holds, never what an entry is.

2. **Four lists page; the others do not.** `GET /api/v1/agents`, `GET /api/v1/hosts`,
   `GET /api/v1/certificates` and `GET /api/v1/revocations` take the parameters of clauses 3 to 6.
   The lists of Configurations, Deployments, Packages, bootstrap certificates and pending
   enrolments are answered whole.

3. **A page is asked for with `limit`; no `limit` keeps the whole array up to a bound.**
   - `limit` is 1 to 1000; a larger value is answered as 1000, and 0, a negative value or one that
     is not a number is answered `400` naming it.
   - A request with neither `limit` nor `cursor` is answered with the whole list while
     it holds at most 10 000 entries after its filters. A longer one is answered `400`, naming
     `limit`; the Server never cuts such a list short.
   - The body is a JSON array of the page's entries, each in the shape it has in the whole list.

4. **The next page is named by a cursor in a `Link` header.** When more entries follow, the
   response carries `Link: <…>; rel="next"`, the same route with the same parameters and a
   `cursor`. The last page carries no `rel="next"`. Every paged and every whole response carries
   `X-Total-Count`, the number of entries that match the filters. A page may hold fewer than
   `limit` entries and still be followed by another.
   - A cursor is opaque. A client never builds or reads one, and passes it back unchanged.
   - A cursor names the sort and filters it was made for. One that does not parse, or arrives with
     another sort or other filters, is answered `400`; one that names an entry since deleted
     continues after where it stood.
   - A cursor carries nothing the caller may not read: it is a position, not a grant. Every page is
     authenticated as any request is.
   - A cursor is a position in the order of clause 5 and holds nothing of the instance that made
     it, so the next page may be asked of any instance.

5. **The order is stable, with a unique tiebreak.**
   - Agents sort by `sort`, one of `name`, `connected`, `health`, `config`, `version`, `os`,
     `seq` and `seen` (default `name`), with `order` `asc` or `desc` (default `asc`), and then by
     Instance UID. The keys mean what the UI's columns of that name show: `name` the display name,
     `health` unhealthy first, then not reported, then healthy.
   - Hosts sort by host id. Certificates sort by the time they were issued, then by issuer and
     serial. Revocations sort by the time they were made, then by id. These lists take no `sort`.
   - An entry whose sort value changes while a client pages may be seen twice or not at all; an
     entry that does not change is seen exactly once.

6. **Filters are applied by the Server, before the page is cut.**
   - Agents take `q`, a case-insensitive substring matched against the same fields the UI's filter
     field searches, except the Configurations a rollout would release to the Agent, which follow
     from the Deployments and not from its record; `selector`, `key=value` pairs separated by commas
     that an Agent matches as a Deployment's Selector matches it
     ([ADR-0033](0033-deployments-and-rollouts.md)): against its effective description, the Host's
     subject values, what it reported, its own labels and its Client Agent's labels, as
     [ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 18 builds it from the Agent's
     record; `connected` and `stale`, `true` or `false`; `service_name`; and on PostgreSQL `holder`,
     an instance id, or `none` for the Agents no instance with a live lease holds
     ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
     clause 13). On the filesystem store there is no holder, and `holder` is answered `400`.
   - Hosts take `q` over the host id and its subject values, and `gateway`, `true` or `false`.
     Certificates and revocations take `q` over subject, serial and host.
   - A parameter a route does not name is answered `400` naming it.

7. **What the UI drew from the whole array, the Server answers.** `GET /api/v1/agents/attributes`
   lists the distinct `key=value` pairs the fleet reports and the distinct `service_name` values,
   each sorted, at most 1000 of each, for the Deployment form's suggestions and the Configuration
   form's types. The Agents tab asks for one page at a time, sends its filter field as `q` and its
   column as `sort`, shows `X-Total-Count` as the count, and leaves a page by the `Link` header.
   Selecting all selects the page on screen.

8. **Every store answers the same queries.** The filter, the order and the cursor are the core's.
   Where the Server holds the fleet in memory, on the filesystem store, the core answers them from
   memory; on PostgreSQL the store answers them through the port's `query_page` (clause 9). Which
   of the two a Server does is the rule of
   [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
   clause 3. The OpenAPI document names every parameter and both headers for the four routes.
   - On PostgreSQL the wire-typed fields of a record are protobuf bytes, which SQL cannot search
     or sort. The core therefore writes, with every write of a record, the columns the queries
     read: the display name and the other sort keys of clause 5, the reported `key=value` pairs,
     the `service_name`, the `host.id` or `host.name` that groups an Agent behind a Gateway, and
     one search text over the fields `q` matches. They are scalars kept as columns, as
     [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
     clause 21 keeps every scalar, written with the record they derive from.
   - Each row also keeps a SHA-256 digest over the columns its derived ones come from. A row
     whose digest does not match them was written without its derived columns: before they
     existed, or by the release before, which serves on the same schema
     ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
     clause 16) and does not write them. Before `query_page` reads, the instance derives such
     rows again from their records, in batches, and writes them, so the answer is the same while
     two releases share the database, but for a small window. The deriving commits each batch in
     a transaction of its own, separate from the page's read, and writes a row only where it is
     unchanged since its batch read it. A row the release before writes after its batch read it,
     or between the deriving and the page's read, may be read with the derived columns it had,
     until a later read derives it again. A batch none of whose rows could be written ends the
     deriving for that read, so the rows after it wait for a later read too.
   - The labels an Agent inherits and the subject values of its Host are joined at read time, from
     the record of its Client Agent and from its Host, so a label change on a Client Agent writes
     that one row and no other.
   - `GET /api/v1/agents/attributes` reads the distinct pairs and types from those columns.

9. **The port of the fleet record gains two reads, `query_page` and `reported_values`.** Beside the
   operations of [ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 2, which it leaves
   as they are, `AgentStore` answers `query_page(filter, sort, cursor, limit)`: the records that
   match `filter`, with their presence, in the order of clause 5 after the position `cursor` names,
   at most `limit` of them, and the number that match.
   - `filter` holds the filters of clause 6, and one the Server uses for its own reads and the
     REST API does not offer: the Host an Agent last arrived over (that ADR's clause 1). Through it
     an instance reads the Agents behind a Gateway, among them the Client Agent whose labels an
     Agent behind it inherits
     ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
     clause 8), and the check of a Gateway's package download reads the Agents that arrived over
     the Gateway's Host rather than the whole fleet.
   - On PostgreSQL it is a read beside `get` and `query`, the reads
     [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
     clause 3 names for a Server that does not hold the fleet in memory. The filesystem adapter
     answers it from the records it loaded, as the core answers the lists there (clause 8).
   - `reported_values(limit)` answers what `GET /api/v1/agents/attributes` lists (clause 7): the
     distinct reported `key=value` pairs and the distinct `service_name` values, each sorted, at
     most `limit` of each. On PostgreSQL it is a `SELECT DISTINCT` for each over the derived
     columns of clause 8, after the rows that need it are derived again; the filesystem adapter
     answers it from the records it loaded, and the core from memory.

**Out of scope:** paging the lists that stay small; a total count that is exact across pages while
the fleet changes; a `rel="prev"` link; a search language beyond substring and Selector; streaming
an export of the whole fleet; the audit record, which the REST API does not serve.

## Alternatives considered

- **A paged envelope on the same route** (`{"items": […], "next_cursor": …}`), as Google's API
  guidelines shape it. It is the clearest shape, but every client that reads the array breaks
  on the next release, at any fleet size. Rejected.
- **A new path, `/api/v2`, for the paged lists.** It follows the path versioning of ADR-0032
  clause 7, but doubles the routes and the document for four lists whose entries do not change,
  and leaves `/api/v1` unbounded. Rejected.
- **Always page, with a default `limit`.** A script that reads the array would silently get
  its first page only. Rejected; a bounded whole answer, refused beyond the bound, says so instead.
- **Offset and limit.** Page numbers are easy to show, but an Agent that connects or changes while
  an operator pages shifts every later entry: some are shown twice, some never, and a deep offset
  costs a scan. Rejected.
- **Keep filtering and sorting in the browser.** It needs the whole fleet in the browser, which is
  what this decision removes. Rejected.
- **A signed cursor.** It would stop a caller from crafting a position, but a position grants
  nothing the caller may not read on page one. Rejected as cost without gain.

## Sources / Prior art

- [Google AIP-158, Pagination](https://google.aip.dev/158): optional page size with a documented
  default, larger values coerced to the maximum, opaque page tokens, an error when a token arrives
  with different parameters, a missing next token as the only end-of-collection signal, and pages
  that may be short.
- [GitHub REST API, Using
  pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api):
  the next page carried in a `Link` header with `rel="next"`, absent on the last page, and a
  `per_page` above the maximum reduced without an error.
- The code as it stands: the routes and `agents` in `crates/fleet-server/src/api.rs`; `render`,
  `renderSelectorOptions` and the refresh intervals in `crates/fleet-server/static/index.html`;
  `matches` in `crates/fleet-server/src/deployments.rs`.

## Consequences

- Positive: the UI's cost per refresh is one page, whatever the fleet's size, and a store that does
  not hold the fleet in memory serves it with one query per page.
- Positive: existing clients keep their arrays while the list holds 10 000 entries or fewer, and
  learn that they must page by a `400` that names `limit`, never by a list cut short.
- Negative / trade-offs: four routes take parameters and two headers, and the bundled UI asks the
  Server per keystroke and per column instead of filtering in place. Selecting all is a page, not
  the fleet.
- Negative / trade-offs: `X-Total-Count` is counted per request; in a database that is a count over
  the matching rows on every refresh.
- Negative / trade-offs: an Agent whose sort value changes while an operator pages can appear twice
  or not at all.
- Follow-ups: an export of the whole fleet for scripts past the bound; what the selection bar acts
  on when an operator means every match rather than a page.

## Enforcement

Tests that carry `Verifies: ADR-0031`:

- Three Agents read with `limit=2` arrive as two pages that together hold each exactly once, the
  first with `Link: …; rel="next"` and the second without, both with `X-Total-Count: 3`.
- A request without `limit` answers the whole array; one whose filters match more than 10 000
  entries is answered `400` naming `limit`.
- `limit=5000` is answered as 1000; `limit=0`, a cursor that does not parse, a cursor sent with
  another `sort` or `q`, and an unknown parameter are each answered `400`.
- `q`, `selector`, `connected`, `stale`, `service_name` and, on PostgreSQL, `holder` each narrow
  the agents to the expected set, and `selector` matches a Host value, a reported attribute, a label
  and a label inherited from the Client Agent as a Deployment's Selector does.
- Each `sort` key orders agents as the UI's column did, ties broken by Instance UID.
- Hosts, certificates and revocations page in their fixed order.
- `GET /api/v1/agents/attributes` answers the distinct pairs and types, and `reported_values`
  answers the same pairs and types on every store adapter, at most its `limit` of each.
- The same tests run against every store adapter the Server ships.
- On PostgreSQL, a label set on a Client Agent narrows a `selector` query to the Agents that
  inherit it, writing no row but the Client Agent's, and a cursor from one instance continues the
  list on another.
- On PostgreSQL, a row whose derived columns were not written with it, as the release before
  writes a report, is found by `q` and `selector` as its record says once `query_page` has read.
- `query_page` filtered on a Gateway's Host answers the Agents that arrived over it and no other,
  on every store adapter.
