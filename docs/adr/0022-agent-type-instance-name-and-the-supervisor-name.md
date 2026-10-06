# ADR-0022: An Agent's type and its instance name are two attributes, and the fleet's own agent is called `supervisor` — the type, the package, the release, the program and its configuration file

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Extends [ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) and
[ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) rather than replacing
either: a Supervisor is still one Agent, and a Selector still matches reported attributes by
equality. What this decides is *what one attribute means*, and what the fleet's own agent is called.
The `name` grammar of [ADR-0010](0010-client-os-service-and-installation-layout.md) and the platform fit of
[ADR-0021](0021-one-platform-vocabulary.md) are untouched.

## Context

A `[[supervisor]]` block's `name` serves two local purposes: it is the directory under
`<supervisor_dir>/` that the Supervisor owns (ADR-0018), and it is the uniqueness key across blocks.
Both are local bookkeeping. Reporting it as the Agent's `service.name` would give it a third,
protocol-visible meaning, and that is wrong in the one place the Baseline is explicit.

**The Baseline reserves `service.name` for the Agent *type*.** `AgentDescription` says it "should be
set to a reverse FQDN that uniquely identifies the Agent **type**, e.g.
`io.opentelemetry.collector`", and names `service.instance.id` separately as what "uniquely
identifies the Agent". This Client reports `service.instance.id` as the instance UID
([`agent.rs:827`](../../crates/client/src/supervisor/agent.rs#L827)). Putting the instance's *name*
into the slot reserved for its type would give one key two meanings.

**The collision is not theoretical: the Managed Process wins it.** A Collector carrying the
`opampextension` reports its own description, and the Supervisor folds it over its own with every
key overwriting except the exempt ones
([`agent.rs:865-874`](../../crates/client/src/supervisor/agent.rs#L865-L874)). An instance name
carried in `service.name` would be replaced the moment the extension connects, by whatever the
Collector's build info says — typically `otelcol-contrib`, identically on every host of that
distribution. The fleet view reads exactly that key
([`fleet.rs:1068`](../../crates/server/src/fleet.rs#L1068)), so three managed Collectors would
become three rows all called `otelcol-contrib`, distinguishable only by the UID printed underneath.
The name an operator chose would not merely be in the wrong field — it would be destroyed by a
process that starts working correctly.

**And the type, which is the more useful of the two, has to be reachable when it matters most.**
Where the extension is absent — the core `otelcol` distribution, every Foreign Agent — nothing
reports a type at all. Without a way to state one, the fleet cannot answer "which of these are
Collectors", and ADR-0016's aiming has no attribute for it. A rollout that should reach the
Collectors and nothing else would have to be expressed through an operator-invented attribute that
every host must carry, which is the per-host wiring ADR-0016 exists to remove. ADR-0021 closed the
same hole for the platform by making the fit mandatory; the type is the other half of "is this
artifact meant for this Agent".

**A reference implementation resolves this the way the Baseline reads.** Bindplane derives its Agent
Type from `dist.name` in the collector's build manifest — "This value is reported by the collector
via OpAMP and can be found in the manifest used to build your collector" — which travels as
`BuildInfo.Command` and is emitted by `opampextension` as the identifying `service.name`. The
instance is `service.instance.id` plus a separate human `agent_name` field that is *not* an
`AgentDescription` attribute at all. So the type is an attribute, the human name is not, and the two
never share a key.

That last detail is the one genuinely open question here, and OpAMP does not answer it: the Baseline
has `service.instance.id` and no notion of a human-readable instance name. Bindplane invented a
product field for it. This Client cannot — its Server learns about an Agent only through the
protocol.

**The fleet's own agent needs a type too, and the word is `supervisor`.** The Baseline recommends a
reverse FQDN for the type — a recommendation this decision declines to enforce, since neither a
Collector's `dist.name` nor a program's file name generally is one, and which
open-telemetry/opamp-spec issue 131 records as a known overload of that key against the resource
semantic conventions. So the value is this project's to choose, and among Collectors, Foreign
Agents and the process that supervises them on a host, the useful answer for the last one is the
role it plays. Everything else follows from taking that answer seriously: what the fleet offers the
thing is named after what the thing is, and so is the thing itself.

## Decision

We will **separate the two meanings into two attributes** — `service.name` carries the Agent
*type*, and a new `service.instance.name` carries the operator's name for the instance — and call
the fleet's own agent **`supervisor`** at every layer where it has a name.

### Type and instance name

1. **`service.name` is the type**, resolved in this order, first hit wins:

   | Source | When |
   |---|---|
   | What the Managed Process reports | the fold already does this, and it is `dist.name` for a Collector |
   | A new optional block key `service_name` | the operator states the type for a process that cannot |
   | The program's file name (`binary` / `command`) | the fallback: `otelcol`, `promtail` |

   The fallback is read from the configuration, never parsed out of a program's output. It is what
   the operator already wrote; extracting a name from `--version` text would be per-tool guesswork,
   and the rule at [`agent.rs:828-831`](../../crates/client/src/supervisor/agent.rs#L828-L831)
   forbids inventing an attribute a Selector could then match. The Baseline says a type "should" be
   a reverse FQDN; neither `dist.name` nor a program file name generally is one, so this ADR treats
   the FQDN as the recommendation it is and does not enforce a shape.

   For the Client's own Agent the type is the constant `supervisor` (clause 6), not the configured
   `name`.

2. **`service.instance.name` is the operator's name for this Agent**, non-identifying: the
   `[[supervisor]]` block's `name` for a Supervisor-backed Agent, the top-level `name` for the
   Client's own. It is reported always, and it is **exempt from the fold** — added beside
   `service.instance.id` in the exemption at
   [`agent.rs:868`](../../crates/client/src/supervisor/agent.rs#L868), for the same reason: a
   Managed Process cannot know what the operator called the Supervisor that owns it, so a value it
   reports under that key is not an improvement on the configured one.

   The key is this project's, not the semantic conventions'. The Baseline explicitly admits "any
   other relevant Resource attributes" and "any user-defined attributes the end user would like to
   associate with this Agent" among non-identifying attributes, which is the licence being used;
   the name is chosen to read as the obvious partner of `service.instance.id` rather than to imply
   a convention that does not exist.

   The Client's own top-level `name` defaults to a display name — spaces and capitals — and
   deliberately not to `supervisor`: a default equal to the type would print the same word in both
   columns of the fleet view, which is the collapse this decision ends. The default itself is
   [ADR-0010](0010-client-os-service-and-installation-layout.md) clause 12's. Nothing resolves a path or a
   service from this key; the ADR-0010 grammar governs the `[[supervisor]]` block names instead.

3. **`AgentView` carries `service_instance_name`**, and the bundled UI's name column shows it with
   the type on the sub-line. The fallback chain is instance name → type → UID, so a row is never
   blank and never collapses onto its neighbours
   ([`fleet.rs:876`](../../crates/server/src/fleet.rs#L876),
   [`index.html:351`](../../crates/server/static/index.html#L351)).

4. **Nothing changes about the block `name` locally.** It stays the directory name and the
   uniqueness key, and it stays bound by the ADR-0010 grammar
   ([`cli.rs:207`](../../crates/client/src/cli.rs#L207)) — lowercase, digits, `-`, no dots. A type
   *may* be a reverse FQDN; an instance name may not, because it is a path component on three
   operating systems. This is why the two cannot share a key even if the semantics allowed it.

5. **A Selector on `service.name` aims at a type.** That is the point, and it is also a break: a
   Selector written against `service.name` as an instance name stops matching. As with the
   `host.arch` change in ADR-0021, this is silent unless stated, so `CHANGELOG.md` names it.

### The fleet's own agent is `supervisor`

6. **The Agent type is `supervisor`** — the constant every Client reports as `service.name`
   ([`agent.rs`](../../crates/client/src/supervisor/agent.rs), `CLIENT_AGENT_TYPE` per ADR-0010
   clause 15), the same on every host, because every Client in a fleet is the same kind of thing and
   that is what a type says.

7. **The package that carries the Client is `supervisor` too.** `[self_update] package` defaults to
   the Agent type ([`config.rs`](../../crates/client/src/config.rs)), which keeps
   [ADR-0017](0017-client-self-update-and-its-consent.md)'s rule intact in letter
   and in substance: the default is not a wildcard, it is the one package that could legitimately be
   this Client, and an offer under any other name is refused and reported. The Set is therefore
   `supervisor` @ version @ `supervisor` — name and type the same string, the way every other
   agent's Set already reads ([ADR-0016](0016-a-package-is-a-versioned-set.md),
   [ADR-0016](0016-a-package-is-a-versioned-set.md)). A Configuration carrying the Client's
   `[[supervisor]]` blocks ([ADR-0029](0029-supervisor-set-from-the-server.md),
   [ADR-0029](0029-supervisor-set-from-the-server.md)) is typed
   `supervisor` as well.

8. **Every release artifact is a `.tar.gz` named `supervisor_<version>_<os>_<arch>`** — after the Set
   the files become, not after the product inside them. `.tar.gz` because it is the container every
   other agent's package already ships as ([ADR-0031](0031-the-glpi-agent.md),
   [ADR-0034](0034-repacked-icinga-2-artifacts.md)): the only one that
   carries the executable bit and unpacks the same way on every platform. `.7z` remains an artifact
   container the Client opens and the packer writes, including encrypted
   ([ADR-0015](0015-package-delivery-for-managed-processes.md)); a *release* has no use for encryption, its
   bytes being published with their checksum beside them.
   - **All four artifacts of a target share the name**, as [ADR-0020](0020-installing-the-client-and-native-installers.md)
     clause 12 requires: the `.tar.gz`, the `.deb`, the `.rpm` and the `.msi` differ in extension alone.
   - **The fields and their separator are [ADR-0019](0019-release-pipeline-and-artifact-names.md)'s**:
     `_` between four fields, the last two exactly what an Agent reports as `os.type` and
     `host.arch`, so an upload reads the platform out of the file name and needs no table.

9. **The program on the host is `supervisor`** (`supervisor.exe` on Windows): the binary Cargo
   builds, the file in every version directory, the member a package artifact carries, and the
   payload under `/usr/libexec`. The version directories follow the same constant,
   `supervisor-<MAJOR.MINOR.PATCH>-<hash>`. So do the log file, the self-check token, and the CLI's
   own name. The service and the `PATH` symlink carry the product's name instead (ADR-0010
   clause 11).

10. **The configuration file is `supervisor.toml`**, and the `--config` default with it. There is no
    fallback to the old name — and because there is none, **a Client that finds a `client.toml`
    beside the `supervisor.toml` it was looking for refuses to start**, naming both paths and the
    command that fixes it. Coming up on defaults there, dialling the development endpoint and
    managing nothing, is the one outcome nobody would see.

11. **The earlier names are not carried across.** No dual-named artifact, no compatibility link in a
    version directory, no reading of the old configuration name. A Client that runs under the old
    names and is offered a `supervisor` release refuses it — the member it asks for by name is not
    in the artifact — and stays on the version it runs. Nothing mis-delivers across the names: a
    type that does not fit is offered nothing, a package name that does not match is refused and
    reported, and a host that is upgraded but not renamed does not start.

12. **What keeps its name, and why:**

    | Stays | Because |
    |---|---|
    | the dpkg and rpm package identity, the MSI `ProductName` and `UpgradeCode` (ADR-0020) | an `apt`, `dnf` or MSI upgrade stays an upgrade rather than becoming a second product beside the first |
    | the Cargo package name `client` | a build-time identifier that never leaves the repository |
    | the OTLP instrumentation scope | it names the library a signal came from, and renaming it would move every operator's dashboards for no gain here |

    The install roots and the names that identify an installation — its paths, its service and its
    package — are the product's, decided by ADR-0010 (clauses 2, 3, 8 and 10).

## Alternatives considered

- **Leave `service.name` as the instance name and add `service.type`.** Smaller and breaks no
  Selector. Rejected: it puts this project's private key in the position the Baseline already
  defined, so a Collector's `opampextension` — which reports `service.name` and knows nothing about
  `service.type` — would keep overwriting the instance name with its type, and the fleet-view
  collapse this decision exists to prevent would survive. It also diverges from the one reference
  implementation checked, for no gain beyond avoiding a rename.
- **Report no human instance name at all**, matching the Baseline exactly: type in `service.name`,
  identity in `service.instance.id`, host in `host.name`. Genuinely tempting, and the most
  conformant. Rejected because a UID is not a name a person can use, and several Supervisors share
  one `host.name`, so an operator managing three Collectors on one host would have three rows with
  the same type, the same host, and two UUIDs to tell apart. The Baseline's own escape hatch for
  exactly this is the user-defined non-identifying attribute clause used in clause 2.
- **Use `host.name` as the instance name.** Free, standard, already reported. Rejected on the same
  case: it is a property of the machine, and Supervisor Mode's whole premise (ADR-0003) is *n*
  Agents on one machine.
- **Put the instance name outside `AgentDescription`, as Bindplane does with `agent_name`.** The
  closest thing to the prior art. Rejected: it needs a channel this project does not have — a
  product-specific field, a header, or a Server-side store keyed by UID — and it makes the name
  invisible to Selectors, when pinning one Agent by name is precisely what ADR-0016 twice offers as
  the way to aim a rollout at a single host.
- **Derive the type from `<program> --version` output** — the first token of
  `otelcol-contrib version 0.114.0` is `BuildInfo.Command`, i.e. exactly `dist.name`. Rejected: the
  version probe works because SemVer is a strict grammar recognisable anywhere in free text
  ([`process.rs:606`](../../crates/client/src/supervisor/process.rs#L606)), and a name has no
  grammar — `Fluent Bit v3.1.0` and `promtail, version 3.0.0 (branch: main)` both defeat "the token
  before *version*". The upstream OpAMP Supervisor had the same option and also declined it,
  bootstrapping through the extension instead. Clause 1's configuration fallback gets the same value
  with none of the guessing.
- **Bootstrap the type by starting the Collector once with a generated opamp-extension-only config**,
  as the upstream Supervisor does within its `bootstrap_timeout`. Rejected for now on
  simplicity-first grounds: it adds a process start to the start path and only works for
  distributions that *have* the extension — which are exactly the ones that already report their
  type through the fold. It solves nothing that clause 1 does not, for the cases that need solving.
  Worth revisiting only if a case appears where the extension exists but connects too late.
- **Keep `opamp-fleet-client` as the fleet's own agent's name everywhere.** Not broken. Rejected:
  the type then repeats a product name the row already carries as its instance name, and the fleet
  cannot say what the Client *is* except by naming the product it happens to be.
- **A reverse FQDN, `io.opamp-fleet.supervisor`**, as the Baseline recommends. Rejected on clause 1's
  reasoning: the shape is a recommendation this project enforces nowhere else, and the string is a
  table column in the operator plane, where the short form is what gets read.
- **Rename the type but not the package, the release or the program** — any one layer taken alone.
  Rejected for each: a package name that is not the type splits ADR-0017's one rule into two names;
  a release named after the product publishes a Set no Client fits; a program called
  `opamp-fleet-client` under a service called `supervisor` is the doubled vocabulary this decision
  exists to end.
- **A transitional release carrying both program names** — the artifact holding the program twice,
  each version directory laying the old name beside the new, the loader accepting either
  configuration file. The only option under which a deployed host updates itself across the rename.
  Rejected deliberately: it creates one file with two names in the packer, the layout, the loader
  and the documentation at once, and ends in a second decision about when to withdraw the old name
  that every host has to survive as well.
- **Rename everything, package identity included** (dpkg/rpm identity, MSI `ProductName`). Rejected:
  it buys consistency where nobody reads and costs every host a second package beside the first, with
  the old one left installed.

## Sources / Prior art

- The Baseline's `AgentDescription`
  ([`opamp.proto:690-727`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L690-L727)) — the
  direct authority for this decision: `service.name` "should be set to a reverse FQDN that uniquely
  identifies the Agent type, e.g. `io.opentelemetry.collector`", `service.instance.id` separately as
  what identifies the Agent, and the non-identifying clause admitting "any user-defined attributes
  that the end user would like to associate with this Agent" that clause 2 relies on. The same
  wording in the [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md).
- [opamp-spec issue #131, "Opamp spec overloads definition of service.name"](https://github.com/open-telemetry/opamp-spec/issues/131)
  — that reading contradicts the resource semantic conventions, where `service.name` is the logical
  name of the service. The recommendation is therefore guidance, not a constraint on the value.
- [OpenTelemetry resource semantic conventions, `service.name`](https://opentelemetry.io/docs/specs/semconv/resource/#service).
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the behavioural oracle for a shipping OpAMP server: an Agent Type identified by `dist.name`
  "reported by the collector via OpAMP", with the human `agent_name` kept as a separate field rather
  than folded into the same attribute.
- [`opampextension` `opamp_agent.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — `createAgentDescription()` sets exactly three identifying attributes (`service.instance.id`,
  `service.name`, `service.version`) and derives the rest from the host. This is what actually
  arrives at a Supervisor Endpoint, and therefore what the fold in clause 2 has to coexist with.
- [OpAMP Supervisor `config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — the comparable configuration surface: `agent.description.identifying_attributes` lets an
  operator state identifying attributes directly, which is the same need clause 1's `service_name`
  key answers, scoped to the one key that has a defined meaning.
- [PR #38809 — Control Collectors with only the OpAMP extension](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38809)
  — the bootstrap mechanism weighed and rejected in Alternatives, and the evidence that upstream
  chose the extension over parsing a CLI flag.
- [OpenTelemetry semantic conventions — `service`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/service.md)
  — checked to confirm what clause 2 admits: the registry defines `service.name`,
  `service.namespace`, `service.version`, and `service.instance.id`, and has **no** attribute for a
  human-readable instance name. `service.instance.name` is this project's, deliberately named to
  parallel the one that exists.
- [ADR-0019](0019-release-pipeline-and-artifact-names.md)'s alternatives entry that rejected a *split*
  container (`.tar.gz` on Unix, `.zip` on Windows) rather than a uniform one.

## Consequences

- Positive: **an operator's name for an Agent survives its Managed Process reporting for itself.**
  A working `opampextension` cannot erase `otelcol-edge-01` or collapse three fleet rows into three
  identically named ones; the cause is removed rather than worked around.
- Positive: **the fleet can be aimed by what an Agent *is*.** A Selector of
  `{"service.name": "otelcol-contrib"}` reaches the Collectors of that distribution and nothing
  else, with no per-host attribute to maintain — the role half of the "is this artifact meant for
  this Agent" question that ADR-0021 answered for the platform. It does not become mandatory the way
  platform fit did, so the mismatched-role package is discouraged, not made impossible.
- Positive: the type is available for the distributions that cannot report it, through configuration
  rather than through a probe that would sometimes be wrong.
- Positive: one fewer overloaded key. The block `name` keeps exactly the local meaning its grammar
  was designed for, and is not a protocol-visible identifier whose value a process can overwrite.
- Positive: one word for one thing, from the Agent type in the fleet view down to the program on the
  host and the file an operator edits. Every artifact name and every attribute of the fleet's own
  agent says `supervisor`.
- Positive: the Client's own release is an ordinary fleet package — same container as every other
  agent's, and a Set whose name and type agree — so nothing about it is a special case, and the
  documented upload procedure produces a Set that actually fits a Client.
- Negative / trade-offs: **a Selector on `service.name` aims at a type and silently stops matching
  if it was written for an instance name.** This is the same failure mode ADR-0021 accepted for
  `host.arch` and it needs the same treatment — a `CHANGELOG.md` entry, because nothing in the
  system can detect it.
- Negative / trade-offs: **`AgentView` has a field whose content changed under an unchanged key.**
  Generated API clients and anything reading `service_name` as a display name see different content
  under an unchanged key, which is worse than a rename would be. A rename of the JSON field is worth
  considering at review.
- Negative / trade-offs: **`service.instance.name` is a key this project invents.** Every invented
  attribute is one a future convention may contradict, and it is matched raw by Selectors, so
  renaming it later breaks them exactly as clause 5 describes for `service.name`.
- Negative / trade-offs: the resolution order in clause 1 means an Agent's reported type can change
  when a Collector gains the extension — from the configured or file-name value to `dist.name`. That
  is the correct value winning, but it is still a Selector that stops matching at an unrelated
  moment.
- Negative / trade-offs: **the term `supervisor` names three things in this project**: the unit
  inside a Client that manages one Managed Process (the specification's *Supervisor*), the Agent
  type, and the program. Documentation keeps them apart by never using the bare word where a file
  is not meant: `service.name = "supervisor"` for the type, `[[supervisor]]` for the block.
- Follow-ups: Server-set labels for staged rollouts are [ADR-0027](0027-server-set-labels.md)'s.
  Still open: whether the Server should warn when a package's Selector names a `service.name` no
  Agent in the fleet reports, which is the type-side equivalent of the fits-no-Agent warning
  ADR-0021 left as a follow-up.
