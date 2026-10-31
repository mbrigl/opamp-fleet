# Ticket: The Client builds its HTTPS clients in one place

## What to build

The builder of an HTTPS client (rustls, no redirects, a timeout) is written five times: in
`packages.rs`, `floors.rs`, `gateway/revocations.rs` and `telemetry.rs` of `fleet-agent`, and in
`opamp::client::connection`, while `crates/fleet-agent/src/tls.rs` says building an HTTP client from
the TLS material is `opamp`'s. TLS 1.3 is enforced once, in `ClientTls::apply`, so this is safe
today and drifts with the next copy.

Give `opamp::client` one function that builds an HTTPS client from a `ClientTls` and its timeouts,
with redirects off, and have the four Client callers use it.

## Acceptance criteria

- [ ] Given each of the four Client callers, when it builds its client, then it calls the one
  `opamp` function.
- [ ] Given a server that answers with a redirect, when any of them fetches, then the redirect is
  not followed, tested once in `opamp`.

## Read first

- ADR-0008, ADR-0012.
- `crates/opamp/src/client/connection.rs` `ClientTls`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
