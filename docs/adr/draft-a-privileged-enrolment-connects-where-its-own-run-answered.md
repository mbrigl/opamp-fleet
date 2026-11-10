# ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered: A privileged enrolment connects where its own run answered

- **Status:** 🟡 proposed
- **Date:** 2026-11-10
- **Deciders:** Markus Brigl
- **Applies to:** the enrolment `service install` and `service config` run in `crates/fleet-agent/src/main.rs` and `crates/fleet-agent/src/service/enrol.rs`, and what the questionnaire and `--endpoint` hand it in `crates/fleet-agent/src/config_init.rs`

## Context

`service install` and `service config` run as root or an Administrator, and the enrolment they
run connects to the Server with the bootstrap identity the administrator provided
([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)
clause 16). It takes the endpoint and the trust (`[tls] ca_file`) from `supervisor.toml`, which it
reads back after the questionnaire or `--endpoint` wrote it. That file and the CA copy beside it
lie in the data root, which the service's account owns
([ADR-0021](0021-the-accounts-the-service-runs-under.md) clause 1).

ADR-0021 clause 11 keeps a privileged step from going through a link the account placed, and the
enrolment now reads the CA file and the bootstrap pair once, bounded, through the held directory.
Clause 11 does not cover the *content* of a file the account owns. An account that is taken over
can rewrite `endpoint` or the CA copy between the write and the read-back. The privileged step
then presents the administrator's bootstrap identity to a Server the account chose.

The gain for such an account is small. The certificate an enrolment obtains lands in the state
directory, which the account owns anyway, and the service connects wherever the file says once it
runs. What the account cannot otherwise do is make root present the administrator's bootstrap
identity to a Server of its choosing. The bootstrap key may be one the account cannot read.

This is backlog measure H21 in [`HARDENING.md`](../HARDENING.md), its second step. It serves Q-1
(secure by default) of [`SPECIFICATION.md`](../SPECIFICATION.md).

Four runs enrol, and they differ in what the administrator said:

- **`--interactive` and `service config`:** the questionnaire asks for the endpoint and the CA;
  its defaults come from the file, and the administrator sees and confirms each.
- **`service install --endpoint` writing a new file:** the administrator names the endpoint; the
  CA comes from beside the program (ADR-0027 clause 17).
- **`service install --endpoint` over an existing file:** the file is kept as it is
  ([ADR-0020](0020-the-installed-service-and-its-state.md)), so the service runs with its
  endpoint, not the one named. The MSI's custom action runs this way on an upgrade that is given
  an endpoint; the Linux packages' `postinst` only prints the command for the administrator.
- **`service install` without either, over an existing file:** nothing is answered; an upgrade
  with a bootstrap identity beside the program enrols with what the file says.

## Decision

We will have a privileged enrolment use the endpoint and CA its own run answered, held in memory,
and read either from `supervisor.toml` only where the run answered none.

- **An answered endpoint** — the questionnaire's answer or `--endpoint` — is the endpoint the
  enrolment connects to. The file is not read back for it.
- **An answered CA** is held as the certificates the run copied beside the configuration, in
  memory. The enrolment trusts these, and does not read the copy again. An answer that names no CA
  leaves the built-in roots, whatever the file says by then.
- **`--endpoint` over a kept file** answers nothing, because the file stands for the service. The
  enrolment must not run when the kept file names another endpoint than `--endpoint`. The install
  then fails and names both endpoints. Where they are the same, the run enrols as one that
  answered nothing.
- **A run that answered nothing** takes both from the file, read once as ADR-0021 clause 11
  says. This is a residual, and the documents that describe the install say so.

**Out of scope:**

- the file the service runs with afterwards, which stays the account's (ADR-0021 clause 21);
- `[tls] cert_file` and `key_file`: an enrolment does not present them while a bootstrap identity
  stands in;
- whether an unanswered run enrols at all.

## Alternatives considered

- **A privileged step never takes the endpoint or the trust from a file in the account's
  directory.** An unanswered `service install` would not enrol, and an upgrade with a bootstrap
  identity beside the program would need `--endpoint` again. Rejected: it changes what automation
  relies on, for a gain the Context weighs as small.
- **Accept the residual and change no code.** The file is the account's word, and the enrolment
  produces nothing the account does not control already. Rejected: where the administrator just
  answered, the run already holds the true values, and using them costs little.
- **Re-read the file and compare it with the answers, refusing on a difference.** It detects a
  rewrite instead of ignoring it. Rejected for a run that wrote the file: holding the answers
  needs no comparison. Chosen for `--endpoint` over a kept file, where no answer was written.
- **`--endpoint` over a kept file enrols with `--endpoint`.** Rejected: the certificate would come
  from one Server while the service connects to the other.
- **`--endpoint` over a kept file counts as no answer, without a check.** Rejected: the MSI passes
  `--endpoint` on an upgrade, so an account that rewrote the file would have root present the
  bootstrap identity to its Server on a common path.

## Sources / Prior art

- [ADR-0021](0021-the-accounts-the-service-runs-under.md) clause 11 and its sources on link
  following in privileged steps; the same reasoning applies to content the account can change.
- CWE-367, Time-of-check Time-of-use (TOCTOU) Race Condition,
  <https://cwe.mitre.org/data/definitions/367.html>: a value checked or written and later
  re-read from a location another party can change.

## Consequences

- Positive: an administrator who answered the questionnaire or gave `--endpoint` enrols with what
  they answered, whatever the account does to the file meanwhile.
- Positive: the enrolment reads no file back for its endpoint or trust in the common runs.
- Negative / trade-offs: the questionnaire and `--endpoint` hand the enrolment the answered
  endpoint and certificates, a change to what `config_init` returns.
- Negative / trade-offs: an unanswered run keeps the residual, and so does the CA of a kept file
  under a matching `--endpoint`. HARDENING.md names both.
- Negative / trade-offs: an upgrade with `--endpoint` over a file that names another endpoint no
  longer enrols. The administrator corrects the file or the flag.
- Follow-ups: whether an unanswered privileged run enrols at all, if the residual proves to matter.

## Enforcement

A test of `crates/fleet-agent` cites this ADR with `Verifies:`. It answers an endpoint and a CA.
It rewrites `supervisor.toml` and the CA copy before the enrolment connects. It observes that the
enrolment connects to the answered endpoint with the answered trust. A second case without answers
observes that the enrolment takes both from the file. A third case runs `--endpoint` over a kept
file that names another endpoint and observes that the enrolment is refused, naming both.
