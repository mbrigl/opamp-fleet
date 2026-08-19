# ADR-0077: The Client's own Agent type is `supervisor` — and so is the package that carries it

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

Changes one value decided by [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md)
point 1 — the constant the Client's own Agent reports as `service.name` — and, with it, the default
of [ADR-0075](0075-the-self-update-consent-stands-unless-it-is-withdrawn.md), which is that same
type. Everything else those two ADRs decide stands, and
[ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) and
[ADR-0030](0030-one-service-name-on-every-platform.md) are untouched: the product, the binary, the
version directories, the release artifact's file name, and the service are all still
`opamp-fleet-client`.

## Context

ADR-0033 separated the two meanings that had shared `service.name`: the key now carries the Agent
*type*, and `service.instance.name` the operator's name for the instance. For a Supervisor-backed
Agent the type says something — `otelcol-contrib`, `promtail`, `icinga2` — because it names the
program that is being managed. For the Client's own Agent it says `opamp-fleet-client`, and that is
the one string on the row that carries no information the operator does not already have several
times over:

- the default instance name is `opamp-fleet-client` as well
  ([`config::default_name`](../../crates/client/src/config.rs#L759)), so a Client with nothing
  configured shows the same word twice in the fleet view — once as its name, once as its type. That
  is precisely the collapse ADR-0033 set out to end, surviving in the default install;
- the service is registered under it (ADR-0030), the version directories are prefixed with it, and
  the release artifact is named after it (ADR-0028).

What the type is *for* is answering "what kind of thing is this" across a fleet of mixed Agents —
that is what a Selector aims at (ADR-0012, ADR-0017), what a Configuration's type fit tests
([ADR-0054](0054-a-configuration-may-state-the-agent-type-it-is-for.md),
[ADR-0057](0057-server-pushed-supervisor-blocks-name-only-client-owned-programs.md)), and what a
package Set is keyed by ([ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md),
[ADR-0052](0052-a-package-is-a-versioned-set.md)). Among Collectors, Foreign Agents and the fleet's
own supervising process, the useful answer for the last one is the role it plays: it is the Agent
that supervises the others on that host.

**The Baseline does not constrain the value.** It says `service.name` "should be set to a reverse
FQDN that uniquely identifies the Agent type, e.g. `io.opentelemetry.collector`" — a recommendation
that ADR-0033 point 1 already declined to enforce, since neither a Collector's `dist.name` nor a
program's file name generally is one. The overload is a known defect of the specification rather
than a rule to obey to the letter: open-telemetry/opamp-spec issue #131 records that this reading of
`service.name` contradicts the resource semantic conventions, where the key is the *logical name of
the service*. So the value is this project's to choose, and it should be chosen for the operator
reading the fleet view.

**The self-update package name follows the type, because ADR-0075 made it the type.** An absent
`[self_update]` section is consent, narrowed to the name of the package that may be written over the
Client — and that name defaults to the Client's own Agent type, "which is what a Set carrying this
Client is keyed by anyway". Renaming the type without renaming the default would split one rule into
two names for no reason an operator could see. It also *can* follow, cheaply: a Set's name is a
label the operator chooses when creating it (`PUT /api/v1/packages/{name}/{type}/{version}`), not
anything baked into the artifact. The release still publishes
`opamp-fleet-client_<version>_<os>_<arch>.7z`, the archive still holds the binary under the name the
install layout gives it, and the same file is uploaded unchanged into a Set now named `supervisor`.
Nothing is repacked and no file is renamed.

## Decision

We will **report `supervisor` as the Client's own Agent type**, and **keep the self-update default
equal to that type**, so the package that carries the Client is named `supervisor` as well.

Bound by this decision:

1. **`CLIENT_SERVICE_NAME` becomes `supervisor`**
   ([`agent.rs:69`](../../crates/client/src/supervisor/agent.rs#L69)). It is still a constant and
   still the same on every host: every Client in a fleet is the same kind of thing, which is what a
   type says. It is no longer the shipped binary's name, and the constant's documentation says so.
2. **The instance name is untouched.** The top-level `name` stays the operator's name for this
   Client, reported as `service.instance.name`, and its default stays `opamp-fleet-client`
   (ADR-0028, ADR-0033 point 2). This ADR changes one *attribute*, not two.
3. **`[self_update] package` still defaults to that type**, so it is now `supervisor`
   ([`config.rs:580`](../../crates/client/src/config.rs#L580)). ADR-0075's rule is unchanged in
   letter and in substance — the default is the Client's own Agent type, it is not a wildcard, it is
   the one package that could legitimately be this Client, and an offer under any other name is
   refused and reported. Only the string changes, because the type did.
4. **The Set that carries the Client is `supervisor` @ version @ `supervisor`** — named and typed
   alike (ADR-0034, ADR-0052). The *artifact* keeps its published file name; only the Set's label on
   the Server changes. A Configuration carrying the Client's `[[supervisor]]` blocks (ADR-0056,
   ADR-0057) is typed `supervisor` too.
5. **Nothing else that reads `opamp-fleet-client` moves** — not `BINARY_FILENAME`, not
   [`layout::COMPONENT`](../../crates/client/src/service/layout.rs#L36), not the service name, not
   the log file stem, not the release artifact's file name, not the MSI, not the OTLP
   instrumentation scope. ADR-0028's hazard about renaming the binary after a release does not
   apply: this ADR renames no file.
6. **It is a breaking change for a deployed fleet, and it is named in `CHANGELOG.md` in the same
   change** — as ADR-0031 and ADR-0033 point 5 did for the same class of silent break. Two things
   stop matching the moment a host comes up on this version: anything aimed at
   `service.name = "opamp-fleet-client"`, and the self-update consent of a host whose `client.toml`
   spells the old package name out. Neither mis-delivers anything — a type that does not fit is
   offered nothing, and a name that does not match is refused and reported — but a host in either
   state is a host the fleet has stopped updating, silently, until it is corrected.

## Alternatives considered

- **Keep the type as `opamp-fleet-client`** — no migration at all, and the status quo is not broken.
  Rejected: it leaves the type column repeating a word the row already carries as its name on every
  default install, and leaves the fleet unable to say what the Client *is* except by naming the
  product it happens to be.
- **Rename the type but keep the self-update default at the product's name** (deriving it from
  `layout::COMPONENT` instead) — no Set has to be renamed, and an existing `client.toml` that spells
  the package out keeps working. Rejected: it splits ADR-0075's one rule into two names, and the
  name it keeps is the *file's*, which is not what a Set is called. The saving is one rename on the
  Server, against a second concept every operator has to hold from then on.
- **A reverse FQDN, `io.opamp-fleet.supervisor`** — what the Baseline recommends. Rejected on
  ADR-0033's reasoning: the shape is a recommendation the project does not enforce anywhere else,
  and this string is a table column in the operator plane, where the short form is what gets read.
- **Rename the default *instance* name instead**, leaving the type alone — it would also end the
  doubled word. Rejected: it fixes the symptom on the wrong attribute, and the instance name is the
  operator's to choose, not the project's.
- **A word other than `supervisor`** — `fleet-client`, `opamp-supervisor`. Not chosen; `supervisor`
  is what the operator asked for and it is the role the process plays on the host. The cost is
  recorded below.

## Sources / Prior art

- [OpAMP specification, `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `service.name` "should be set to a reverse FQDN that uniquely identifies the Agent type".
- [opamp-spec issue #131, "Opamp spec overloads definition of service.name"](https://github.com/open-telemetry/opamp-spec/issues/131)
  — the specification's use of the key contradicts the resource semantic conventions, where
  `service.name` is the logical name of the service. The recommendation is therefore treated as
  guidance, not as a constraint on the value.
- [OpenTelemetry resource semantic conventions, `service.name`](https://opentelemetry.io/docs/specs/semconv/resource/#service)
  — the logical name of the service, defaulting to the executable's name only in the absence of
  anything better.
- Bindplane's Agent Type, already surveyed in ADR-0033: the type is derived from the distribution's
  own name and is a separate field from the human-readable Agent name — the split this project
  implements, and evidence that the type is expected to say *what kind of agent*, not *which
  product build*.

## Consequences

- **Positive:** the type column answers "what is this" for the Client's own Agent instead of
  repeating the product name; on a default install the fleet view no longer shows the same string as
  both name and type; a Selector or a Set can aim at the fleet's supervising Agents as a class
  without borrowing the product's name.
- **Positive:** one name, not two. The type, the Set that carries the Client, and the default
  `[self_update] package` are the same string — the property ADR-0075 relied on, kept rather than
  quietly abandoned.
- **Negative — a vocabulary collision, and it is real.** The specification defines **Supervisor** as
  a unit *inside* a Client that manages exactly one Managed Process. This decision uses the same
  word for the Client's own Agent as a whole, which is the thing that *runs* those Supervisors. In
  prose the two will be confusable, so documentation writes the type as `service.name = "supervisor"`
  or "the type `supervisor`" and never as a bare noun where a Supervisor could be meant. An operator
  reading the fleet view sees one row typed `supervisor` per host and its Managed Processes typed
  after their programs, which is legible; the risk is in the writing, not in the view.
- **Negative — a migration on every existing deployment, in two parts.** On the Server: Selectors
  matching `service.name = "opamp-fleet-client"`, Configurations typed with it (including the
  `[[supervisor]]`-set Configuration of ADR-0056), and the Set that carries the Client have to be
  re-typed, and that Set re-created under the name `supervisor`. On the hosts: a `client.toml` that
  names `package = "opamp-fleet-client"` explicitly refuses everything else, so it has to be edited
  — and `[self_update]` is not server-manageable (ADR-0057 admits only `[[supervisor]]` blocks), so
  that edit belongs to configuration management or a hand, not to the fleet. Hosts that never wrote
  the section — the common case since ADR-0075 — need nothing.
- **Negative — ordering matters for the self-update itself.** The Set that delivers *this* version
  must still be named and typed `opamp-fleet-client`, because that is what a host reports and
  consents to while it is being offered. The Set for every later version is `supervisor` @
  `supervisor`. Getting it wrong delivers nothing rather than the wrong thing, but it delivers
  nothing silently.
- **Follow-ups:** if more of the fleet's own components ever report as Agents, whether they share
  this type or take one each is an open question; nothing here decides it.
