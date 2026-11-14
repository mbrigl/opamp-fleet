# A kept file's CA file is read as the administrator's

**Kind:** defect, accepted for now.

**Observed.** A privileged enrolment reads the paths the questionnaire offered from
`supervisor.toml` through no link at their name, and any other path as an administrator's path,
links followed outside the account's directories (`read_once` in
`crates/fleet-agent/src/service/enrol.rs`, `Offered` in `crates/fleet-agent/src/config_init.rs`,
ADR-0021 clause 11). A run that asks nothing, `service install` over a kept `supervisor.toml` with
or without a matching `--endpoint`, offers nothing, so it reads the kept file's `[tls] ca_file` as
the administrator's. The service's account wrote that path, and the administrator never saw it: a
stronger case than an offered default. Only certificates are kept from what is read, and a file
holding none is refused, so the read discloses no key.

**Idea.** `Written` names the kept file's `[tls]` paths as offered when the run answered nothing,
so `read_once` reads them `ByAccountFile`; one test of a link at a kept `ca_file` under
`service install` without questions.

**From.** Review of revision-2026-11-12/01 on `feat/bugfixes` (2026-11-13).
