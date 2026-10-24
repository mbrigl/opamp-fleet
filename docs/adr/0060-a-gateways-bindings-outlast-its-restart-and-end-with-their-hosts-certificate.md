# ADR-0060: A Gateway's bindings outlast its restart, end with their host's certificate, and never give way to another host

- **Status:** 🟡 proposed
- **Date:** 2026-10-24
- **Deciders:** Markus Brigl
- **Applies to:** the bindings of `instance_uid`s to hosts in `crates/fleet-agent/src/gateway/policy.rs` and `crates/fleet-agent/src/gateway/cache.rs`, their store under `<state_dir>/gateway-bindings` and its loading and writing in `crates/fleet-agent/src/gateway/`, the downstream refusal while that store cannot be read in `crates/fleet-agent/src/gateway/mod.rs`, and every document that tells an operator what a Gateway keeps on disk or how an Agent behind one is bound to its host
- **Supersedes:** [ADR-0039](0039-through-a-gateway-a-csr-proves-the-certificate-it-rests-on.md) clause 6

## Context

A Gateway carries other hosts' Agents, and the Server sees only the Gateway's certificate, which
speaks for any Agent ([ADR-0036](0036-a-bootstrap-certificates-subject-is-carried-into-the-host-certificate.md)
clause 2). What keeps one host behind a Gateway from reporting for another host's Agent is the
Gateway's binding: [ADR-0039](0039-through-a-gateway-a-csr-proves-the-certificate-it-rests-on.md)
clause 6 binds an `instance_uid` to the host of the first downstream report for it, and clause 5
forwards a report only for an `instance_uid` bound to the host that sent it. Through that binding
a host receives an Agent's configurations and package offers and speaks for it to the Server.

The binding lasts only "since the Gateway started". The bindings are held in memory
(`crates/fleet-agent/src/gateway/policy.rs:1`), so after every restart of the Gateway — an update,
a reboot, a crash — whichever host reports an `instance_uid` first binds it. A host with a valid
member certificate that knows another host's `instance_uid` can report for it first and take over
that Agent: its configurations, its offers, its reports to the Server. The Server cannot see it,
because the Gateway's certificate speaks for any Agent, and the re-keying that protects an
`instance_uid` on a direct connection (ADR-0036 clause 2) does not reach behind a Gateway. The
same opening exists without a restart: past the Gateway's total of 1 000 000 bindings, a new
binding takes the place of the least recently reported idle binding of any host (`policy.rs:87`),
so a host that fills the table can free another host's binding and claim it. Both are named as
residuals in `docs/HARDENING.md` and ADR-0039 (*Out of scope*, *Consequences*).

ADR-0039 rejected persisting the bindings "for now", because the Gateway would keep state on disk
that it must defend. Since then the state directory has come to be defended as a whole: no
privileged step follows a link in it ([ADR-0054](0054-no-privileged-install-step-follows-a-link-the-account-placed.md)),
files are written through the no-follow writer, and where a process account is registered the
Client's own files are out of its reach ([ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
clause 6). The state directory already holds the Client's private key; a binding table is less
to defend than that.

The specification's quality goal Q-1 asks that the Fleet be secure by default, G-15 that a
Gateway scales connections and not identities, and G-17 that an Agent is identified. A binding
that any member can claim after a routine restart serves none of them.

Persisting the bindings brings a consequence of its own. In memory, a binding to a host that no
longer reports — decommissioned, or enrolled anew under another host — ended at the next restart.
Kept on disk and never given way to another host, it would hold the `instance_uid` forever, and
its Agent, moved to a new host, would be refused behind that Gateway. A binding therefore needs an
end of its own: the host's ability to report, which ends with its certificate.

## Decision

We will keep a Gateway's bindings in its state directory across restarts, end a host's bindings
when the certificate it last presented has expired or is revoked, and never let a binding take the
place of another host's.

1. **Restates ADR-0039 clause 6. Downstream, a host receives only what the Gateway relayed to its
   own Agents.** The Gateway serves `GET /api/v1/packages/{agent_type}/{version}/file` on its
   downstream listener, behind the handshake and the revocation verdict of its `/v1/opamp`: a
   certificate from `client_ca_file` is required, a revoked one is answered `401` and every
   request is answered `503` while the Gateway holds no current list (ADR-0015 clause 14,
   ADR-0039 clause 4).
   - **Binding.** An `instance_uid` is bound to the host of the certificate of the first
     downstream connection that reports for it, as the Server binds an `instance_uid` to the host
     that first reports it (ADR-0036 clause 2), and the binding is kept across the Gateway's
     restarts (clause 2) until it ends (clause 3). The host is the one the certificate names
     (`urn:opamp-fleet:host:<id>`). A certificate that names none is a host of its own: the host
     the Server derives from its issuer and serial when it first renews it (ADR-0036 clause 2),
     computed the same way. So it neither shares a binding with another certificate nor takes one
     over, and the certificate its renewal brings names the same host. The derived host binds for
     routing only: offers and downloads are recorded and served only for a host a certificate
     names (*Offers*, *Requests*). A host holds at most `max_carried_agents` bindings, the number
     ADR-0015 clause 4 bounds a connection by. Past that its report for a new `instance_uid` takes
     the place of that host's own least recently reported binding whose `instance_uid` has neither
     a downstream route nor a current offer, so a host whose Agents come and go is never shut out
     by its own past; when it has none, the report binds nothing, and that is logged. The Gateway
     holds at most 1 000 000 bindings in all; past that a new binding takes the place of the
     reporting host's own binding as above, and never another host's: when the reporting host has
     none to give, it binds nothing, and that is logged. Each report refreshes when its binding
     was last reported. What a report that binds nothing does is ADR-0039 clause 5's.
   - **Offers.** An offer relayed to an `instance_uid` is recorded only when the downstream
     connection it is relayed over presents a certificate of the host the `instance_uid` is
     bound to; any other is not recorded, and that is logged. An Agent's current offer is the
     files of the last recorded `packages_available` relayed to it: a later one replaces it, one
     that names no Server-hosted file removes it, a message without `packages_available` leaves
     it standing (ADR-0026 clause 36), and a downstream disconnect does not end it. The offers
     held are bounded by the offers the Server makes through the Gateway. The offers are kept in
     memory only, indexed by host; a restart ends them, and the Server's next offer records them
     again.
   - **Requests.** A request is served only when the Gateway holds the artifact and a current
     offer of it (the same path and query, byte for byte) to an `instance_uid` bound to the host
     the request's certificate names. A certificate that names no host receives nothing. While an
     offered artifact is being fetched, or waits for a fetch slot, the request is answered `503`
     with `Retry-After: 30` instead of waiting, so a Client's read timeout does not run out while
     the Gateway fetches. An offered artifact that is neither held nor being fetched is fetched on
     a request at most once per relayed offer of it — the next relayed offer re-arms it — and that
     request is answered `503` with `Retry-After: 30`. After a failed fetch it is not fetched
     again until ADR-0026 clause 42 says so.
   - **Everything else** is answered `404`, with the same status, headers and body whether no such
     offer was recorded, it was recorded for another host, the artifact could not be fetched or
     verified, it is too large for the cache, or the request names nothing the route knows.

   The bound on what a host receives is ADR-0035 clause 2's, applied to what the Server sent
   through the Gateway. The binding, the `404` for an artifact too large or not fetched, and the
   `503` while a fetch runs are the Gateway's own. Together they are the download refusal of
   ADR-0039 clause 4.

2. **The bindings are kept in `<state_dir>/gateway-bindings`, written before they are relied on.**
   - **What is kept.** Each binding's `instance_uid` and host, and per host the end of the
     certificate it last presented — its `notAfter`, and its issuer and serial as the revocation
     list names a certificate (the SHA-256 of the issuer's DER name and the serial in lowercase
     hex). When a binding was last reported is kept too, but written in batches, at most once a
     minute and at shutdown: a restart may lose the order of the last minute's reports, never a
     binding.
   - **A new binding is durable before its report is forwarded.** The Gateway appends it to the
     store and syncs it before it forwards the report that made it, and before that report's
     connection or exchange becomes the Agent's route (ADR-0039 clause 5). A binding that cannot be
     written is not made: the report binds nothing, is treated as ADR-0039 clause 5 treats such a
     report, and the failure is logged. A binding given up for another of the same host's, and a
     binding that ends (clause 3), is appended as its removal.
   - **The store is a journal, compacted.** Entries are appended, one line each, and the Gateway
     rewrites the store whole from what it holds when it starts and when the journal has grown to
     twice the lines it holds, as a new file synced and renamed over the old. It is written through
     the no-follow writer (ADR-0054) and readable and writable by the Client's account alone
     (`0600`); a process account gets nothing of it (ADR-0059 clause 6).
   - **An unreadable store refuses every downstream Agent.** At start the Gateway reads the store
     without following a link. Where it exists and is not a regular file the Client's account
     owns, is not `0600`, or holds a line it cannot parse beyond a last line cut short — which it
     drops, as a write cut off by a crash — the Gateway carries no downstream Agent: it answers
     every downstream report and download `503`, logs at every start and once an hour why and
     that removing the file lets it start empty, and its own Supervisors and its upstream
     connection go on. A store that does not exist is an empty one.

3. **A host's bindings end with the certificate it last presented.** Each report records, for its
   host, the certificate the downstream connection presented. When that certificate's `notAfter`
   has passed, or the revocation list the Gateway holds (ADR-0039 clause 4) lists its issuer and
   serial, every binding of the host ends and is removed from the store, and that is logged with
   the host and how many bindings went. The Gateway checks at start, at every revocation list it
   takes, and once an hour. A host whose certificate is renewed presents the new one at its next
   report, which moves the end forward; a host that can no longer report — its certificate expired
   or revoked — keeps no `instance_uid` bound, and the `instance_uid`'s next report binds it anew.

**Out of scope:** binding at the Server the `instance_uid`s a Gateway carries, so that a member
connecting directly cannot claim one first; a command to end a binding by hand; keeping the offers
across a restart; a rate per downstream member certificate at the Gateway; a Gateway's bindings
moved to another Gateway.

## Alternatives considered

- **Keep the bindings in memory, as ADR-0039 decided.** Every restart of a Gateway opens every
  Agent behind it to whichever member host reports for it first. Rejected: a routine update is
  enough to open it, and the Server cannot see it.
- **Bind at the Server.** The Server would bind each `instance_uid` behind a Gateway to a
  downstream host, which closes the restart and the direct claim alike, but it has to learn the
  downstream host from the Gateway's word, which ADR-0039 rejected as a header the Server would
  have to trust, and it needs a protocol extension and a Server store. Not taken now; it stays a
  follow-up, and a Gateway that keeps its bindings does not stand in its way.
- **Keep a binding for a fixed time without a report** (for example 60 days). A host's ability to
  report ends with its certificate, which the Gateway sees; a fixed time is either longer than that
  and holds an `instance_uid` for a host that cannot use it, or shorter and lets a quiet Agent's
  binding go. Rejected.
- **End a binding only by an operator's command.** It needs a privileged interface on the Gateway
  host, and an `instance_uid` stays held until someone notices. Rejected in favour of the
  certificate's end; a command stays possible later.
- **Let a new binding still take another host's idle binding at the total, as ADR-0039 decided.**
  That is the takeover path without a restart. Rejected; the cost is that a Gateway full of
  bindings binds nothing new for a host that has none of its own to give, which is logged.
- **Start empty when the store cannot be read, with a warning.** It reopens the restart window
  that this decision closes, at the moment an attacker who can damage the file chooses. Rejected
  in favour of failing closed for downstream Agents alone.
- **Rewrite the whole store at every new binding.** At up to 1 000 000 bindings that is a large
  write per report that binds; an append and a sync is one line. Rejected in favour of the
  journal.
- **A database (SQLite).** One more dependency and file format to defend for one table with two
  operations. Rejected.

## Sources / Prior art

- OpenSSH `known_hosts` (`ssh(1)`, *VERIFYING HOST KEYS*): trust on first use is only as good as
  the record of first use; a record that is forgotten makes every connection the first.
- RFC 7435 (*Opportunistic Security*), which names trust on first use: a binding made at first
  contact protects only after that contact, and only as long as it is kept.
- The append-only journal with periodic compaction of Redis' AOF rewrite and of LevelDB's log;
  the sync before acknowledging is the write-ahead rule of PostgreSQL's WAL (*Reliability and the
  Write-Ahead Log*).
- RFC 5280 section 4.1.2.5 (*Validity*) and section 5 (CRLs): a certificate past `notAfter` or on
  the issuer's list is not to be relied on, so neither is anything bound to it.
- This repository: [ADR-0036](0036-a-bootstrap-certificates-subject-is-carried-into-the-host-certificate.md)
  clause 2 (the Server's own first-report binding, which it persists),
  [ADR-0039](0039-through-a-gateway-a-csr-proves-the-certificate-it-rests-on.md) clauses 4–6,
  [ADR-0054](0054-no-privileged-install-step-follows-a-link-the-account-placed.md),
  [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
  clause 6, `crates/fleet-agent/src/gateway/policy.rs`, `docs/HARDENING.md`.

## Consequences

- Positive: a Gateway restart no longer lets a member host take over another host's Agent, and
  neither does a host that fills the binding table. A host that leaves the fleet frees its
  `instance_uid`s when its certificate ends, at the latest after one certificate life (30 days by
  default, ADR-0021 clause 9), at once when it is revoked.
- Negative / trade-offs: the Gateway keeps a file it must defend, writes and syncs one line per new
  binding, and refuses every downstream Agent while that file is damaged, until an administrator
  removes it. An Agent that moves to a new host behind the same Gateway is refused there until the
  old host's certificate ends or is revoked; revoking the old host's certificate frees it at the
  Gateway's next revocation list. A Gateway at its total binds nothing new for a host that has no
  binding of its own to give. Offers still end with a restart.
- Follow-ups: binding at the Server what a Gateway carries; a command to end a binding by hand; a
  rate per downstream member certificate.

## Enforcement

Tests that will carry `Verifies: ADR-0060`:

- A binding made before a Gateway restart holds after it: a second host's report for that
  `instance_uid` is refused (`403` over plain HTTP, dropped over WebSocket) and the first host's
  report is forwarded.
- A report that makes a new binding is forwarded only after the binding is in the store; when the
  store cannot be written, the report binds nothing and is not forwarded.
- At the Gateway's total, a report from a host without an idle binding of its own binds nothing,
  and no other host's idle binding is removed.
- A host whose last certificate has passed `notAfter`, or is on a revocation list the Gateway
  takes, loses every binding, in memory and in the store, and another host's report then binds
  the `instance_uid`.
- A store that is a link, not `0600`, owned by another account, or holds an unparsable line before
  its last makes the Gateway answer downstream reports and downloads `503` while its upstream
  connection and its own Supervisors go on; a last line cut short is dropped and the rest loaded.
- The journal is compacted at start and at twice its live lines, and the compacted store loads to
  the same bindings.
