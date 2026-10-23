# ADR-0056: The lists that grow with the fleet are paged by cursor and filtered on the Server

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the list routes `GET /api/v1/agents`, `GET /api/v1/hosts`, `GET /api/v1/certificates` and `GET /api/v1/revocations` in `crates/fleet-server/src/api.rs` and the OpenAPI document generated from them, the queries the fleet and the revocation register answer for them in `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/revocation.rs` and every store adapter behind them, the Agents and Hosts tabs of the bundled UI in `crates/fleet-server/static/index.html`, and every document or script that reads one of these lists
- **Supersedes:** [ADR-0024](0024-the-fleet-record.md) clause 1

## Context

`GET /api/v1/agents` answers every Agent the Server knows, as one JSON array
([ADR-0024](0024-the-fleet-record.md) clause 1). The bundled UI fetches that array every two
seconds, then filters, sorts and counts it in the browser: one text field matches a substring
across names, status, OS, platform, versions, attributes, labels, capabilities and package
statuses, and the column headers sort by name, connection, health, configuration status, version,
OS, sequence number and last seen. The same array feeds the Deployment form's attribute
suggestions and the Configuration form's type suggestions. `GET /api/v1/hosts`,
`GET /api/v1/certificates` and `GET /api/v1/revocations` answer whole arrays as well, polled every
five seconds by the Hosts tab. Each of the four grows with the fleet: one entry per Agent, per host,
per certificate issued and not yet expired, per revocation.

The other lists do not. Configurations, Deployments, Packages and bootstrap certificates are made
by an operator, one at a time, and stay few. The enrolment queue holds at most 1024 requests
([ADR-0046](0046-one-bootstrap-certificate-holds-at-most-a-quarter-of-the-enrolment-queue.md)).

The target architecture, the working note "server split" (D7), aims at about 100 000 Agents, held
in PostgreSQL and served by several Server instances, with paging and filtering on the Server so
that the UI no longer loads the whole fleet. At that size a whole array is tens of megabytes every
two seconds per open browser, and an instance that does not hold every record in memory would have
to read all of them for each request. An offset into a list that changes while it is read skips or
repeats entries; a cursor that names the last entry read does not, and costs the same on every
page.

The REST API is the fleet's stable contract (G-5), versioned by path
([ADR-0023](0023-configurations-and-the-rest-api.md) clause 11). Scripts and external UIs read these
arrays today. Whatever pages them must not change what such a client receives while the list is
small enough to send whole, and must not cut a list short without the client noticing.

## Decision

We will let the four lists that grow with the fleet be read a page at a time, by an opaque cursor
over a stable order, with filters the Server applies, while a request that asks for no page keeps
today's whole array up to a bound.

1. **Restates ADR-0024 clause 1. One record per Agent, keyed by Instance UID.** It holds what the
   Agent last reported (`sequence_num`, capabilities, description, health, effective
   configuration, remote-config, connection-settings, and package statuses, available
   components), the transport of the last report, `last_seen_ms`, a queued restart
   (`restart_pending`), the assignments rollout acts wrote
   ([ADR-0025](0025-rollout-and-what-reaches-an-agent.md)), a mirror of its labels, and the
   runtime facts `connected` and the owning connection. `GET /api/v1/agents` returns the records
   as `AgentView`s, a page at a time or whole as clause 3 says.

2. **Four lists page; the others do not.** `GET /api/v1/agents`, `GET /api/v1/hosts`,
   `GET /api/v1/certificates` and `GET /api/v1/revocations` take the parameters of clauses 3 to 6.
   The lists of Configurations, Deployments, Packages, bootstrap certificates and pending
   enrolments are answered whole, as today.

3. **A page is asked for with `limit`; no `limit` keeps the whole array up to a bound.**
   - `limit` is 1 to 1000; a larger value is answered as 1000, and 0, a negative value or one that
     is not a number is answered `400` naming it.
   - A request with neither `limit` nor `cursor` is answered with the whole list, as today, while
     it holds at most 10 000 entries after its filters. A longer one is answered `400`, naming
     `limit`; the Server never cuts such a list short.
   - The body is the JSON array it is today, holding the page's entries. Nothing in an entry's shape
     changes.

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

5. **The order is stable, with a unique tiebreak.**
   - Agents sort by `sort`, one of `name`, `connected`, `health`, `config`, `version`, `os`,
     `seq` and `seen` (default `name`), with `order` `asc` or `desc` (default `asc`), and then by
     Instance UID. The keys mean what the UI's columns mean today: `name` the display name, `health`
     unhealthy first, then not reported, then healthy.
   - Hosts sort by host id. Certificates sort by the time they were issued, then by issuer and
     serial. Revocations sort by the time they were made, then by id. These lists take no `sort`.
   - An entry whose sort value changes while a client pages may be seen twice or not at all; an
     entry that does not change is seen exactly once.

6. **Filters are applied by the Server, before the page is cut.**
   - Agents take `q`, a case-insensitive substring matched against the same fields the UI's filter
     field searches today; `selector`, `key=value` pairs separated by commas that an Agent matches
     as a Deployment's Selector matches it, labels included
     ([ADR-0034](0034-a-deployment-carries-configurations-and-is-the-one-thing-rolled-out.md));
     `connected` and `stale`, `true` or `false`; and `service_name`.
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
   The filesystem store answers them from the records it holds in memory; a store that does not
   hold every record answers them in its own query. The OpenAPI document names every parameter and
   both headers for the four routes.

**Out of scope:** paging the lists that stay small; a total count that is exact across pages while
the fleet changes; a `rel="prev"` link; a search language beyond substring and Selector; streaming
an export of the whole fleet; the audit record, which the REST API does not serve.

## Alternatives considered

- **A paged envelope on the same route** (`{"items": […], "next_cursor": …}`), as Google's API
  guidelines shape it. It is the clearest shape, but every client that reads the array today breaks
  on the next release, at any fleet size. Rejected.
- **A new path, `/api/v2`, for the paged lists.** It follows the path versioning of ADR-0023
  clause 11, but doubles the routes and the document for four lists whose entries do not change,
  and leaves `/api/v1` unbounded. Rejected.
- **Always page, with a default `limit`.** A script that reads the array today would silently get
  its first page only. Rejected; a bounded whole answer, refused beyond the bound, says so instead.
- **Offset and limit.** Page numbers are easy to show, but an Agent that connects or changes while
  an operator pages shifts every later entry: some are shown twice, some never, and a deep offset
  costs a scan. Rejected.
- **Keep filtering and sorting in the browser.** It needs the whole fleet in the browser, which is
  what this decision removes. Rejected.
- **A signed cursor.** It would stop a caller from crafting a position, but a position grants
  nothing the caller may not read on page one. Rejected as cost without gain.

## Sources / Prior art

- The working note "server split", section D7 (server-side paging and filtering; the UI no longer
  loads the whole fleet), recorded outside the ADRs.
- [Google AIP-158, Pagination](https://google.aip.dev/158): optional page size with a documented
  default, larger values coerced to the maximum, opaque page tokens, an error when a token arrives
  with different parameters, a missing next token as the only end-of-collection signal, and pages
  that may be short.
- [GitHub REST API, Using pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api):
  the next page carried in a `Link` header with `rel="next"`, absent on the last page, and a
  `per_page` above the maximum reduced without an error.
- The code as it stands: the routes and `agents` in `crates/fleet-server/src/api.rs`; `matches`,
  `sortVal`, `render`, `renderSelectorOptions` and the refresh intervals in
  `crates/fleet-server/static/index.html`; `matches` in `crates/fleet-server/src/deployments.rs`.

## Consequences

- Positive: the UI's cost per refresh is one page, whatever the fleet's size, and a store that does
  not hold the fleet in memory serves it with one query per page.
- Positive: existing clients keep their arrays while the list holds 10 000 entries or fewer, and
  learn that they must page by a `400` that names `limit`, never by a list cut short.
- Negative / trade-offs: four routes take parameters and two headers, and the bundled UI changes
  from filtering in place to asking per keystroke and per column. Selecting all is a page, not the
  fleet.
- Negative / trade-offs: `X-Total-Count` is counted per request; in a database that is a count over
  the matching rows on every refresh.
- Negative / trade-offs: an Agent whose sort value changes while an operator pages can appear twice
  or not at all.
- Follow-ups: an export of the whole fleet for scripts past the bound; what the selection bar acts
  on when an operator means every match rather than a page.

## Enforcement

Tests that will carry `Verifies: ADR-0056`:

- Three Agents read with `limit=2` arrive as two pages that together hold each exactly once, the
  first with `Link: …; rel="next"` and the second without, both with `X-Total-Count: 3`.
- A request without `limit` answers the whole array; one whose filters match more than 10 000
  entries is answered `400` naming `limit`.
- `limit=5000` is answered as 1000; `limit=0`, a cursor that does not parse, a cursor sent with
  another `sort` or `q`, and an unknown parameter are each answered `400`.
- `q`, `selector`, `connected`, `stale` and `service_name` each narrow the agents to the expected
  set, and `selector` matches a label as a Deployment's Selector does.
- Each `sort` key orders agents as the UI's column did, ties broken by Instance UID.
- Hosts, certificates and revocations page in their fixed order.
- `GET /api/v1/agents/attributes` answers the distinct pairs and types.
- The same tests run against every store adapter the Server ships.
