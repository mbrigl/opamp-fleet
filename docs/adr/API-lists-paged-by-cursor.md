# API-lists-paged-by-cursor: The lists that grow with the fleet are paged by cursor and filtered on the Server

- **Status:** 🟢 accepted
- **Date:** 2026-11-07
- **Deciders:** Markus Brigl
- **Applies to:** the list routes `GET /api/v1/agents`, `GET /api/v1/hosts`, `GET /api/v1/certificates` and `GET /api/v1/revocations` in `crates/fleet-server/src/api.rs` and the OpenAPI document generated from them, the queries the fleet and the revocation register answer for them in `crates/fleet-server/src/fleet/views.rs`, `crates/fleet-server/src/revocation.rs` and every store adapter behind them, the Agents and Hosts tabs of the bundled UI in `crates/fleet-server/static/index.html`, and the reads `query_page` and `reported_values` of the `AgentStore` port in `crates/fleet-server/src/agent_store.rs` and its PostgreSQL adapter with the columns it reads, and every document or script that reads one of these lists

## Context

`GET /api/v1/agents` answers the Agents the Server knows as `AgentView`s
([SRV-fleet-record-agents-hosts §1](SRV-fleet-record-agents-hosts.md)).
`GET /api/v1/hosts`, `GET /api/v1/certificates` and `GET /api/v1/revocations` answer arrays as
well. Each of the four grows with the fleet: one entry per Agent, per host, per certificate issued
and not yet expired, per revocation.

The bundled UI reads them all the time. The Agents tab refreshes every two seconds. It has one
filter field that matches a substring across many fields, and column headers that sort by name,
connection, health, configuration status, version, OS, sequence number and last seen. The
Deployment form suggests attributes and the Configuration form suggests types, both drawn from the
fleet. The Hosts tab polls hosts, certificates and revocations every five seconds. Answered whole,
and filtered, sorted and counted in the browser, each response would grow with the fleet.

The other lists do not grow with it. An operator makes Configurations, Deployments, Packages and
bootstrap certificates one at a time, and they stay few. The enrolment queue holds at most 1024
requests ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)).

A fleet is up to about 100 000 Agents (the specification's *Assumptions*), held in PostgreSQL and
served by several Server instances. At that size a whole array is tens of megabytes every two
seconds per open browser. An instance that does not hold every record in memory would read all of
them for each request. An offset into a list that changes while it is read skips or repeats
entries. A cursor that names the last entry read does neither, and costs the same on every page.

The REST API is the fleet's stable contract (G-5), versioned by path
([SRV-configurations-compose-one-map §7](SRV-configurations-compose-one-map.md)). Scripts and
external UIs read these arrays. Paging must not change what such a client receives while the list
is small enough to send whole. It must not cut a list short without the client noticing.

This ADR adds to [SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md) and changes
none of its clauses. Its clause 1 says what an Agent's entry is, not how many a response holds. Its
port gains two reads beside those of its clause 2 (clause 9 here).

## Decision

We will let the four lists that grow with the fleet be read a page at a time:

- by an opaque cursor over a stable order;
- with filters the Server applies;
- a request that asks for no page gets the whole array up to a bound.

1. **A page holds the entries the whole list holds, in the same shape.**
   - `GET /api/v1/agents` answers the records as `AgentView`s, as
     [SRV-fleet-record-agents-hosts §1](SRV-fleet-record-agents-hosts.md) decides.
   - The other three lists answer the entries they answer whole.
   - Paging changes how many entries a response holds, never what an entry is.

2. **Four lists page, and the others do not.**
   - `GET /api/v1/agents`, `GET /api/v1/hosts`, `GET /api/v1/certificates` and
     `GET /api/v1/revocations` take the parameters of clauses 3 to 6.
   - The Server answers the lists of Configurations, Deployments, Packages, bootstrap certificates
     and pending enrolments whole.

3. **A client asks for a page with `limit`. Without `limit`, the Server answers the whole array up
   to a bound.**
   - `limit` is 1 to 1000. The Server answers a larger value as 1000.
   - The Server answers `400` naming `limit` for 0, a negative value, or a value that is not a
     number.
   - The Server answers a request with neither `limit` nor `cursor` with the whole list while it
     holds at most 10 000 entries after its filters.
   - The Server answers a longer list `400`, naming `limit`. It never cuts such a list short.
   - The body is a JSON array of the page's entries, each in the shape it has in the whole list.

4. **A cursor in a `Link` header names the next page.**
   - When more entries follow, the response carries `Link: <…>; rel="next"`. The link is the same
     route with the same parameters and a `cursor`.
   - The last page carries no `rel="next"`.
   - Every paged and every whole response carries `X-Total-Count`, the number of entries that
     match the filters.
   - A page may hold fewer than `limit` entries and still be followed by another.
   - A cursor is opaque. A client must not build or read one, and passes it back unchanged.
   - A cursor names the sort and filters it was made for. The Server answers `400` to a cursor that
     does not parse, or that arrives with another sort or other filters.
   - A cursor that names an entry since deleted continues after where that entry stood.
   - A cursor carries nothing the caller may not read. It is a position, not a grant. The Server
     authenticates every page as any request.
   - A cursor is a position in the order of clause 5 and holds nothing of the instance that made
     it. A client may ask any instance for the next page.

5. **The order is stable, with a unique tiebreak.**
   - Agents sort by `sort`, then by Instance UID. `sort` is one of `name`, `connected`, `health`,
     `config`, `version`, `os`, `seq` and `seen`, with the default `name`. `order` is `asc` or
     `desc`, with the default `asc`.
   - The keys mean what the UI's columns of that name show. `name` is the display name. `health`
     sorts unhealthy first, then not reported, then healthy.
   - Hosts sort by host id.
   - Certificates sort by the time they were issued, then by issuer and serial.
   - Revocations sort by the time they were made, then by id.
   - Hosts, certificates and revocations take no `sort`.
   - An entry whose sort value changes while a client pages may be seen twice or not at all. An
     entry that does not change is seen exactly once.

6. **The Server applies the filters before it cuts the page.**
   - Agents take these filters:
     - `q`, a case-insensitive substring. It matches the fields the UI's filter field searches,
       except the Configurations a Rollout would release to the Agent. Those follow from the
       Deployments, not from the Agent's record.
     - `selector`, `key=value` pairs separated by commas. An Agent matches it as it matches a
       Deployment's Selector
       ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)). The match
       runs against the Agent's effective description.
       [SRV-fleet-record-agents-hosts §18](SRV-fleet-record-agents-hosts.md) builds it from the
       record: the Host's subject values, what the Agent reported, its own Labels, and its Client
       Agent's Labels.
     - `connected` and `stale`, each `true` or `false`.
     - `service_name`.
     - On PostgreSQL, `holder`: an instance id, or `none` for the Agents that no instance with a
       live lease holds
       ([SRV-instance-opens-chosen-planes §13](SRV-instance-opens-chosen-planes.md)). The
       filesystem store has no holder, and there the Server answers `holder` with `400`.
   - Hosts take `q` over the host id and its subject values, and `gateway`, `true` or `false`.
   - Certificates and revocations take `q` over subject, serial and host.
   - The Server answers `400` naming a parameter that the route does not name.

7. **The Server answers what the UI drew from the whole array.**
   - `GET /api/v1/agents/attributes` lists the distinct `key=value` pairs the fleet reports and the
     distinct `service_name` values. Each list is sorted and holds at most 1000 entries. The
     Deployment form's suggestions and the Configuration form's types read it.
   - The Agents tab asks for one page at a time. It sends its filter field as `q` and its column
     as `sort`. It shows `X-Total-Count` as the count, and leaves a page by the `Link` header.
   - Selecting all selects the page on screen.

8. **Every store answers the same queries.**
   - The filter, the order and the cursor belong to the core.
   - On the filesystem store, where the Server holds the fleet in memory, the core answers them
     from memory.
   - On PostgreSQL the store answers them through the port's `query_page` (clause 9).
   - [SRV-state-stores-contract-pair §3](SRV-state-stores-contract-pair.md) decides which of the
     two a Server does.
   - The OpenAPI document names every parameter and both headers for the four routes.
   - On PostgreSQL the wire-typed fields of a record are protobuf bytes, which SQL cannot search
     or sort. So with every write of a record, the core writes the columns the queries read:
     - the display name and the other sort keys of clause 5;
     - the reported `key=value` pairs;
     - the `service_name`;
     - the `host.id` or `host.name` that groups an Agent behind a Gateway;
     - one search text over the fields `q` matches.

     These are scalars kept as columns, as
     [SRV-state-stores-contract-pair §21](SRV-state-stores-contract-pair.md) keeps every scalar.
     They are written with the record they derive from.
   - Each row also keeps a SHA-256 digest over the columns its derived columns come from.
     - A row whose digest does not match them was written without its derived columns. It was
       written before those columns existed, or by the release before. That release serves on the
       same schema
       ([SRV-instance-opens-chosen-planes §16](SRV-instance-opens-chosen-planes.md)) and does
       not write them.
     - Before `query_page` reads, the instance derives such rows again from their records, in
       batches, and writes them. So the answer is the same while two releases share the database,
       except for a small window.
     - The deriving commits each batch in a transaction of its own, separate from the page's read.
     - It writes a row only where the row is unchanged since its batch read it.
     - The release before may write a row after its batch read it, or between the deriving and the
       page's read. The page may then read that row with the derived columns it had, until a later
       read derives it again.
     - A batch in which the deriving wrote no row ends the deriving for that read. The rows after
       it wait for a later read.
   - The read joins the Labels an Agent inherits and the subject values of its Host at read time.
     It takes them from the record of its Client Agent and from its Host. So a Label change on a
     Client Agent writes that one row and no other.
   - `GET /api/v1/agents/attributes` reads the distinct pairs and types from those columns.

9. **The port of the fleet record gains two reads, `query_page` and `reported_values`.**
   - The operations of [SRV-fleet-record-agents-hosts §2](SRV-fleet-record-agents-hosts.md) stay
     as they are.
   - `AgentStore` answers `query_page(filter, sort, cursor, limit)`. It returns the records that
     match `filter`, with their presence, in the order of clause 5 after the position `cursor`
     names. It returns at most `limit` of them, and the number that match.
   - `filter` holds the filters of clause 6. It also holds one filter for the Server's own reads,
     which the REST API does not offer. That filter is the Host an Agent last arrived over (that
     ADR's clause 1).
     - Through it an instance reads the Agents behind a Gateway. Among them is the Client Agent
       whose Labels an Agent behind it inherits
       ([SRV-instance-opens-chosen-planes §8](SRV-instance-opens-chosen-planes.md)).
     - The check of a Gateway's package download reads through it the Agents that arrived over the
       Gateway's Host, not the whole fleet.
   - On PostgreSQL, `query_page` is a read beside `get` and `query`. Those are the reads
     [SRV-state-stores-contract-pair §3](SRV-state-stores-contract-pair.md) names for a Server
     that does not hold the fleet in memory.
   - The filesystem adapter answers `query_page` from the records it loaded, as the core answers
     the lists there (clause 8).
   - `reported_values(limit)` answers what `GET /api/v1/agents/attributes` lists (clause 7). It
     returns the distinct reported `key=value` pairs and the distinct `service_name` values. Each
     is sorted and holds at most `limit` entries.
   - On PostgreSQL, `reported_values` runs a `SELECT DISTINCT` for each over the derived columns of
     clause 8. It runs after the rows that need it are derived again.
   - The filesystem adapter answers `reported_values` from the records it loaded, and the core
     answers it from memory.

**Out of scope:**

- paging the lists that stay small;
- a total count that is exact across pages while the fleet changes;
- a `rel="prev"` link;
- a search language beyond substring and Selector;
- streaming an export of the whole fleet;
- the audit record, which the REST API does not serve.

## Alternatives considered

- **A paged envelope on the same route** (`{"items": […], "next_cursor": …}`), as Google's API
  guidelines shape it. It is the clearest shape, but it breaks every client that reads the array,
  at any fleet size. Rejected.
- **A new path, `/api/v2`, for the paged lists.** It follows the path versioning of
  SRV-configurations-compose-one-map §7. But it doubles the routes and the document for four lists
  whose entries do not change, and leaves `/api/v1` unbounded. Rejected.
- **Always page, with a default `limit`.** A script that reads the array would silently get its
  first page only. Rejected: a bounded whole answer, refused beyond the bound, says so instead.
- **Offset and limit.** Page numbers are easy to show. But an Agent that connects or changes while
  an operator pages shifts every later entry: some are shown twice, some never. A deep offset costs
  a scan. Rejected.
- **Keep filtering and sorting in the browser.** It needs the whole fleet in the browser, which is
  what this decision removes. Rejected.
- **A signed cursor.** It would stop a caller from crafting a position. But a position grants
  nothing the caller may not read on the first page. Rejected as cost without gain.

## Sources / Prior art

- [Google AIP-158, Pagination](https://google.aip.dev/158): an optional page size with a documented
  default, larger values coerced to the maximum, opaque page tokens, an error when a token arrives
  with different parameters, a missing next token as the only end-of-collection signal, and pages
  that may be short.
- [GitHub REST API, Using
  pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api):
  the next page carried in a `Link` header with `rel="next"`, absent on the last page, and a
  `per_page` above the maximum reduced without an error.
- The code: the list routes in `crates/fleet-server/src/api.rs` and `crates/fleet-server/src/api/`;
  the order and cursor in `crates/fleet-server/src/listing.rs`; `render`, `renderSelectorOptions`
  and the refresh intervals in `crates/fleet-server/static/index.html`; `matches` in
  `crates/fleet-server/src/deployments.rs`.

## Consequences

- Positive: the UI's cost per refresh is one page, whatever the fleet's size. A store that does not
  hold the fleet in memory serves it with one query per page.
- Positive: existing clients keep their arrays while the list holds 10 000 entries or fewer. They
  learn that they must page by a `400` that names `limit`, never by a list cut short.
- Negative / trade-offs: four routes take parameters and two headers. The bundled UI asks the
  Server per keystroke and per column instead of filtering in place. Selecting all is a page, not
  the fleet.
- Negative / trade-offs: the Server counts `X-Total-Count` per request. In a database that is a
  count over the matching rows on every refresh.
- Negative / trade-offs: an Agent whose sort value changes while an operator pages can appear twice
  or not at all.
- Follow-ups: an export of the whole fleet for scripts past the bound; what the selection bar acts
  on when an operator means every match rather than a page.

## Enforcement

These tests carry `Verifies: API-lists-paged-by-cursor`:

- `the_fleet_pages_by_cursor_with_no_agent_twice_and_none_missing` in
  `crates/fleet-server/tests/rest_api.rs` reads 2 500 Agents with `limit=1000`. They arrive as
  three pages, each but the last followed by its `Link`, every one with `X-Total-Count`, no Agent
  twice and none missing (clauses 3, 4, 5).
- `without_limit_the_whole_fleet_comes_back_up_to_the_bound` answers the whole array without
  `limit`. A list longer than the bound is answered `400` naming `limit` (clause 3).
- `a_list_request_it_cannot_answer_is_refused_naming_what` answers a `limit` above 1000 as 1000.
  It answers `400` to each of these (clauses 3, 4, 6):
  - `limit=0`;
  - a cursor that does not parse;
  - a cursor sent with another `sort` or `q`;
  - an unknown parameter.
- `a_nul_is_refused_in_a_list_parameter_a_cursor_and_a_label` refuses a NUL in a list parameter or
  cursor, and holds a cursor made by hand to a position (clause 4).
- `a_cursor_names_its_position_for_its_own_sort_and_filters_alone` in
  `crates/fleet-server/src/api/paging.rs` holds a cursor to its sort and filters (clause 4).
- `pages_follow_each_other_in_a_stable_order` in `crates/fleet-server/src/listing.rs` pages without
  a gap or a repeat in either direction, ties broken by the tiebreak (clause 5).
- `the_list_of_agents_filters_sorts_and_pages_on_the_filesystem` and
  `the_list_of_agents_filters_sorts_and_pages_alike_on_postgres` in
  `crates/fleet-server/src/fleet/views.rs` hold `q`, `selector`, `connected`, `stale`,
  `service_name` and every `sort` key to the same answer on both stores (clauses 5, 6, 8). The
  PostgreSQL test also covers:
  - a row the release before wrote without its derived columns, derived again before it is read;
  - a Label set on a Client Agent, which writes that one row and narrows a `selector` to the Agents
    that inherit it.
- `the_search_finds_an_agent_under_its_canonical_platform`,
  `the_view_names_the_platform_the_offer_reads` and
  `an_agents_listed_record_and_its_view_show_the_same_values` hold what the list searches and sorts
  by to what the view shows (clauses 1, 6, 8).
- `the_hosts_the_certificates_and_the_revocations_page_in_their_fixed_order` in
  `crates/fleet-server/tests/mutual_tls.rs` pages hosts, certificates and revocations in their fixed
  order, filters applied before the page is cut (clauses 5, 6).
- `the_attributes_the_fleet_reports_are_answered_for_the_forms` answers the distinct pairs and
  types (clause 7).
- `the_agent_port_contract_holds_on_the_filesystem` and `the_agent_port_contract_holds_on_postgres`
  in `crates/fleet-server/src/postgres/agents.rs` hold `query_page` and `reported_values` to the
  same answer on every store adapter, at most `limit` of each (clauses 8, 9).
- `every_write_of_a_record_writes_its_derived_columns_with_it` holds the derived columns and their
  digest to the statement that writes the record (clause 8).
- `a_page_names_a_holder_only_while_its_lease_is_live` holds `holder` and `connected` to the lease
  (clause 6).
- `an_agents_view_names_its_holder_and_the_list_filters_by_it` in
  `crates/fleet-server/tests/instances.rs` filters by `holder` and continues a cursor from one
  instance on another (clauses 4, 6).
- `a_stored_label_in_the_supervisors_space_matches_nothing_in_the_list` holds a `selector` and the
  inherited Labels to the Labels the list may match (clause 6).
- `a_gateways_download_and_an_agent_behind_it_read_the_gateways_agents_and_not_the_fleet` in
  `crates/fleet-server/src/fleet/delivery.rs` reads, through `query_page` filtered on a Gateway's
  Host, the Agents that arrived over it and no other (clause 9).
