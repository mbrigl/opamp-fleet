# ADR-0034: A package states the Agent type it is built for, and reaches no Agent of another

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Extends [ADR-0017](0017-selector-targeted-packages.md) and
[ADR-0031](0031-per-platform-package-variants.md) rather than replacing either: Selector aiming and
its specificity rule survive unchanged, and so does platform fit. This adds a second mandatory fit
step in front of both. It builds directly on
[ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md), which is what first
gave every Agent a type worth matching.

## Context

ADR-0031 closed one half of "is this artifact meant for this Agent" and said so plainly: a variant
whose Platform is not the Agent's is dropped "before anything else is considered", because "a binary
that cannot run on the machine it is sent to is not a targeting mistake to be resolved by
precedence — it is not a candidate". The reasoning was that the Selector *can* express the platform
but is opt-in, "and the failure mode of forgetting it is the worst one this system has".

**The other half is still exactly where the platform was.** A package's Agent type is expressible
only as a Selector pair, and a Selector starts empty and matches everything
([`packages.rs:139`](../../crates/server/src/packages.rs#L139)). Offer resolution filters on aim and
platform and nothing else
([`resolve()`](../../crates/server/src/packages.rs#L1127-L1138)):

```rust
let Some(platform) = Platform::reported(description) else { return Ok(Vec::new()) };
let fitting = packages.values()
    .filter(|p| matches(&p.selector, description))   // aim — opt-in
    .filter_map(|p| p.variants.get(&platform)...)    // fit — mandatory
```

So an operator who uploads a Promtail artifact for the right platform and forgets the Selector has
it downloaded, verified, unpacked and swapped over the **Collector's** binary on every consenting
host. The health gate catches it (ADR-0015) — the process will not stay up and is rolled back — but
that is the fleet-wide outage window ADR-0031 refused to accept for the platform, still available
for the role. The argument that settled it there settles it here: a rule that is only ever right
when remembered is not the rule this needs.

**Until now there was nothing to match against.** This is why ADR-0031 could not simply have done
both at once. `service.name` carried the *instance* name, different on every host and destroyed by
any Collector that reported its own type, so "the Agent type" was not a fact the Server could read.
ADR-0033 made it one: `service.name` is now the type, resolved from what the Managed Process reports,
else the block's `service_name`, else the program's file name — so every Agent this Client presents
reports a type, always.

**And the Client's own binary is protected by exactly one thing.** ADR-0020 makes `[self_update]
package` the whole of the protection, because "a package with an empty Selector reaches every
consenting Agent, so without a name to match, the first fleet-wide Collector artifact someone
uploads would be installed over the Client and take the host out of reach". That check lives on the
host being protected and compares a *package name* — an operator who names a Collector package
`opamp-fleet-client` defeats it. A Server-side type fit is an independent second guard on the one
case where the blast radius is the Client itself.

## Decision

We will make an **Agent type** a property of a package, and offer a package only to Agents whose
reported `service.name` equals it.

1. **The type belongs to the name, not to the bytes.** `Package.service_name`, shared by every
   variant, exactly as the Selector is — a type is platform-independent, and a value repeated on
   each of five artifact uploads is five chances for them to disagree. ADR-0031 states the rule this
   follows: "a request naming *bytes* names the Platform they are for; a request aiming the
   *package* does not."

   It is set through its own sub-resource, `PUT /api/v1/packages/{name}/type`, beside the Selector's.
   On disk it joins `<name>.json`, the file that already holds what belongs to the name.

2. **Fit before aim, in two steps, neither optional.** Offer resolution drops every package whose
   `service_name` is not the Agent's reported `service.name`, then every variant whose Platform is
   not the Agent's, and only then runs ADR-0017's aiming over what is left. Type first because it is
   the cheaper comparison and the coarser cut.

   The type is compared **raw**, with no canonicalisation table. ADR-0031 could canonicalise because
   the semantic conventions enumerate operating systems and architectures; there is no canonical set
   of Agent types and inventing one would mean this Server having an opinion about every collector
   distribution that exists. The value an Agent reports is the value to write.

3. **A package with no type set is offered to nobody**, and the package view says so. This is
   deliberately *not* ADR-0031's "refused at startup": there, the unset state was dangerous, because
   a stored package without a Platform would have had to mean either "every platform" (the hole) or
   nothing at all. Here the unset state is already the safe one — no type, no offer — so refusing to
   boot would buy no guarantee and cost every operator an outage to migrate. Fail closed, stay up,
   and say why.

   The guarantee is therefore absolute without the store ever being rejected: **a package is offered
   only to an Agent of its type.** An untyped package is inert, not permissive.

4. **An Agent that reports no `service.name` fits nothing**, the same rule ADR-0031 applies to a
   missing platform, and for the same reason: "unknown type, so anything goes" would put the
   mismatched-binary failure straight back. Every Client this project ships reports a type since
   ADR-0033; a foreign OpAMP client that does not is told so on its fleet row rather than left with
   a rollout that never starts.

5. **The Client's self-update gains a second, independent guard.** The Server will not offer a
   package typed `otelcol-contrib` to an Agent reporting `opamp-fleet-client`, whatever it is named,
   and the ADR-0020 name check still runs on the host. Neither replaces the other: one is the
   Server refusing to send, the other the Client refusing to install.

## Alternatives considered

- **Leave it to the Selector and document it.** `{"service.name": "otelcol-contrib"}` already does
  the filtering today, at zero cost. Rejected on ADR-0031's own words: it is opt-in, and forgetting
  it bricks an agent on every host of every other type. This is the same alternative that ADR
  rejected for the platform, and nothing about the role makes the argument weaker — if anything the
  blast radius is larger, since a wrong-platform binary fails to exec while a wrong-role binary may
  start, run, and quietly collect nothing.
- **Default the type to the package name.** A package named `otelcol-contrib` would be for Agents of
  that type unless overridden, which needs no new input at all for the shape operators already use
  and would have made this change invisible. Rejected: it is right most of the time and silently
  wrong for `otelcol-canary`, `collector-3.1.0-rc`, or any name chosen for the rollout rather than
  the target — and a mechanism whose entire purpose is "not optional" cannot rest on a guess that is
  usually correct. ADR-0031 rejected the structurally identical "optional Platform, empty meaning
  every platform".
- **Require the type as a query parameter on the artifact upload**, as ADR-0031 does for `os`/`arch`.
  Fewer requests, no window in which a package exists untyped. Rejected on point 1: five platform
  uploads under one name would state the type five times and could disagree, and resolving that
  disagreement (last writer wins? refuse a mismatch?) is a rule this project would rather not have.
  ADR-0031 moved the Selector off the bytes route for precisely this reason; the type is the
  Selector's kind of thing, not the artifact's.
- **Refuse an untyped package at startup**, mirroring ADR-0031 point 8 exactly. The consistent
  choice, and it was the starting assumption. Rejected once the asymmetry was clear: an untyped
  package here is already inert, so the startup refusal protects nothing that point 3 does not, and
  it costs a Server-down migration on every deployment — the second one in two ADRs. Consistency of
  mechanism is worth less than not taking a fleet offline for a state that is safe.
- **Match the type as a prefix or a pattern** (`otelcol*` covering `otelcol` and `otelcol-contrib`).
  Rejected: those are genuinely different binaries with different components, which is the whole
  reason the distinction exists, and a matching language is a decision that outlives its convenience.
  Two packages, two types.
- **Canonicalise types through an alias table**, as ADR-0031 does for platforms. Rejected in point 2:
  there is no authority to canonicalise against, and a table this project maintained would encode its
  opinion of every distribution in the world.

## Sources / Prior art

- [ADR-0031](0031-per-platform-package-variants.md) — the direct model for this decision: the
  fit-before-aim step, the "reports nothing fits nothing" rule, and the reasoning that a Selector
  which must be remembered is not a guarantee. This ADR is that argument applied to the other half of
  the question, and it deliberately diverges on one point (startup refusal) with the reason stated.
- [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) — what made a type
  matchable at all, and the resolution order (`opampextension`'s `dist.name` → the block's
  `service_name` → the program's file name) that guarantees every Agent this Client presents has one.
- The Baseline's `AgentDescription`
  ([`opamp.proto:690`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L690)) — `service.name`
  as "a reverse FQDN that uniquely identifies the Agent type", which is the attribute this fit reads
  and the reason it is the right one to read.
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — "the packages that are available on the Server **for this Agent**", and the note that what a
  package contains "is Agent type-specific and is outside the concerns of the OpAMP protocol". The
  protocol names the concept, declines to model it, and leaves the Server to decide — which is the
  licence for deciding it here.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the comparable product models an Agent Type as a first-class object that a collector reports via
  `dist.name`, and hangs what an agent may receive off it rather than off free-form labels. Checked
  as evidence that type-as-a-property, not type-as-a-tag, is the shape that holds up in a shipping
  fleet manager.
- [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md) — cited by
  ADR-0031 for the platform; the same structure is the counter-example here, since an image index
  discriminates on platform alone and leaves "what is this image for" to the name. That is the design
  this ADR declines, because a container name is chosen by whoever pulls it and a package name is
  chosen by whoever uploads it.

## Consequences

- Positive: **the worst remaining operator mistake stops being available.** A package can no longer
  be installed over an Agent of another type, whatever its Selector says or fails to say, so the
  fleet-wide outage window ADR-0031 closed for the platform is closed for the role.
- Positive: **the Client's own binary is protected twice, on both sides of the wire** — the Server
  will not offer it a foreign artifact and the Client will not install one. ADR-0020's single point
  of protection stops being single.
- Positive: an untyped package is inert rather than fleet-wide, so the failure mode of an incomplete
  upload changes from "installed everywhere" to "installed nowhere, and the view says why".
- Positive: the Selector goes back to being purely about *aim* — rings, environments, canaries —
  instead of carrying the role as a pair that also perturbs the specificity count. A ladder of
  `{}` / `{rollout: canary}` no longer has to reserve a pair for the type.
- Negative / trade-offs: **a fourth breaking change to the v1 package contract.** Every package needs
  its type set before it is offered again, `PackageView` grows a field, and any script that uploads
  and expects delivery needs one more request. Existing stores load and keep working, but every
  package in them is inert until typed — a rollout in flight at upgrade time stops.
- Negative / trade-offs: **a mistyped type is a silent no-op.** ADR-0031 could catch a typo by
  canonicalising and echoing the pair back; there is no equivalent here, so `otelcol-contib` is
  indistinguishable from a type no Agent happens to run yet. The package view showing "offered to 0
  Agents" is the mitigation, and it is weaker than the platform's.
- Negative / trade-offs: **an Agent's type can change under a stable configuration.** ADR-0033 notes
  that a Collector gaining the `opampextension` switches its reported type from the program's file
  name to `dist.name`; with this ADR that also switches which packages reach it. The change is
  correct in both cases and surprising in both.
- Negative / trade-offs: one more thing to get right before a first rollout works, in a system whose
  reason for existing is that rollouts should be easy. Type, platform, Selector, consent — four
  things now, and only the last is derived rather than stated.
- Follow-ups: **warn when a type fits no Agent in the fleet.** ADR-0031 left the platform equivalent
  as a follow-up; here it matters more, because there is no canonicalisation to catch a typo, and it
  is the only cheap defence against the silent no-op above. Also open, and now more clearly worth
  it: the Server-set labelling described in ADR-0033's follow-ups, since with the role handled by
  type fit the Selector's remaining job is almost entirely rollout rings — which is exactly the thing
  that should not live in a file on each host.
