# ADR-0049: The MSI's endpoint dialog is prefilled with the development default — interactively only

- **Status:** 🟡 proposed
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

ADR-0046 gave the MSI an endpoint dialog whose field is empty, and made an empty answer legal:
"configure later", the deferred-configuration install that only warns. Its rationale leans on
ADR-0027's finding that a Client with no configuration dials the development default
(`ws://127.0.0.1:4320/v1/opamp`) forever — "a defect, not a default" — and concludes that "a
package installed on a thousand hosts must not manufacture that state a thousand times."

The thousand-hosts case never sees the dialog: fleet deployments run `msiexec /qn` through Intune,
Group Policy or SCCM, where the endpoint arrives as `ENDPOINT=` on the command line. The person the
dialog *does* face is evaluating or developing against a local Server, and today the field greets
them empty — the one value they will type is the development default the Client already knows.

## Decision

We will prefill the `ENDPOINT` property with the development Server in its HTTP form,
`http://localhost:4320/v1/opamp`, **in the UI sequence only** (a `SetProperty` with
`Sequence="ui"`, conditioned on the property being unset and the product not yet installed).

- A silent install that names no `ENDPOINT` behaves exactly as before: no configuration is
  written, the install warns and defers — unattended fleet deployment cannot acquire the
  development default by omission.
- A value given on the `msiexec` command line wins over the prefill; clearing the field remains
  the "configure later" answer.
- The prefilled value names the same host, port and path as the loader's `default_endpoint()`,
  but with the `http://` scheme — the scheme selects the transport (ADR-0008), and HTTP polling
  is the operator's explicit choice for the click-through install.
  `crates/client/tests/msi_exe_command.rs` holds the value to the loader's endpoint rule and to
  this string.

## Alternatives considered

- **Keep the field empty (ADR-0046 as decided)** — safest, but the interactive install then asks a
  question whose most common answer for the audience actually facing the dialog is a string the
  product already knows; explicitly requested away by the operator.
- **A static `Property` default (both sequences)** — one line, but it changes silent-install
  semantics: `/qn` without `ENDPOINT=` would pin every unattended host to localhost, precisely the
  state ADR-0046 refuses to manufacture.
- **Prefill the loader's `default_endpoint()` (`ws://127.0.0.1:4320/v1/opamp`) verbatim** — one
  value defined once, and the transport an unconfigured Client picks anyway. Rejected by the
  operator in favour of the `http://` form; the divergence (two development defaults, a scheme
  apart) is accepted and pinned by test rather than left to drift.

## Sources / Prior art

- ADR-0046 (the dialog and the empty-endpoint semantics), ADR-0027 (the development-default
  half-state), ADR-0008 (scheme selects transport).
- Windows Installer: type 51 (set-property) custom actions scheduled in `InstallUISequence` do not
  run under `/qn`; `Secure` public properties cross the UAC elevation boundary — the standard
  mechanism for UI-only defaults.

## Consequences

- Positive: the local-evaluation install is a click-through; the interactive and unattended paths
  keep their distinct semantics; a test holds the MSI's value to the loader's endpoint rule.
- Negative / trade-offs: an operator interactively installing on a production host and clicking
  through without reading now gets a `client.toml` pinned to localhost rather than a warning — the
  narrow slice of ADR-0046's concern this decision consciously accepts. And a click-through
  install runs HTTP polling where an unconfigured Client would have chosen WebSocket — two
  development defaults, a scheme apart.
- Follow-ups: none. The Linux packages have no interactive channel to mirror this in (ADR-0046
  records endpoint preseeding there as rejected).
