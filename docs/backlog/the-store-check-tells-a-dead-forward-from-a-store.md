# The store check tells a dead forward from a store

**Kind:** defect, check.

**Observed.** The Dev Container forwards the host's PostgreSQL (5432) and S3 store (9100) with socat
listeners (`.devcontainer/forward-otlp.sh`) that accept a TCP connection even while the host stack
is down, and close it at once. A probe by TCP connect says "up", and the store tests then fail with
"connection closed" while creating the schema, which reads as a code error. The script's header
says the opposite (line 19: "a connection made while it is down fails"). `scripts/check-store.sh`
on `main` has no probe at the store's protocol.

**What the old branch had.** Revision ticket 19 of `feat/revision-2026-10-31`, open, not worked:
before `check-store.sh` runs the tests, probe the named store at its protocol, a PostgreSQL startup
exchange or an HTTP request to the S3 endpoint, and fail with "the store at <address> does not
answer" when nothing answers behind the listener; and correct the header of `forward-otlp.sh`.
