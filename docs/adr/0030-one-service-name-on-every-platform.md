# ADR-0030: One service name on every platform — `opamp-fleet-client`, with the instance as a suffix

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **service-label** decision of
[ADR-0010](0010-client-os-service-and-cli.md) — `io.opamp-fleet.client.<instance>`, and the
human-readable Windows display name it promised — and with it decision point 5 of
[ADR-0028](0028-the-client-is-named-opamp-fleet-client.md), which kept that label deliberately.
Everything else in both is untouched: the per-instance isolation, the install layout, the shipped
binary's name.

## Context

ADR-0010 chose one reverse-DNS label, `io.opamp-fleet.client.<instance>`, and handed it to the
`service-manager` crate. What an operator actually sees is not that string, because the crate renders
a label through **two** different functions and the backends do not agree on which to use:

| | Rendering | Today's name |
|---|---|---|
| systemd | `{organization}-{application}` | `opamp-fleet-client.default.service` |
| launchd | `{qualifier}.{organization}.{application}` | `io.opamp-fleet.client.default` |
| Windows SCM | the same qualified form | `io.opamp-fleet.client.default` |

So the Client already has two names in the field, and neither was chosen — both fell out of how a
four-token label happens to be split (`io` / `opamp-fleet` / `client.default`). That the systemd unit
reads `opamp-fleet-client.default` is a coincidence of `organization` and `application` being joined
with a hyphen; it is not the shipped binary's name arriving there on purpose. An operator who learns
one name and moves to another platform has learned the wrong one, and every runbook has to say both.

ADR-0028 gave the shipped artifact, the installed file, and the version directory a single name for
exactly this reason, and explicitly left the label alone as "identity of a registered service"
rather than a file name. That reasoning holds for *why the label may differ from the binary*; it does
not answer why the label should differ **from itself** across three platforms.

Two constraints shape what is possible.

**The two renderings can never coincide for a compound label.** One joins with `-`, the other with
`.`, so any label with more than one part produces two different strings. There is, however, a case
where they agree: with no qualifier and no organization, `to_qualified_name()` and `to_script_name()`
both reduce to the application alone. A label that is a **single token — no dots** — therefore
renders identically on all three backends. That is the whole mechanism this ADR needs, and it costs
nothing: no fork per platform, no unit file of our own.

**A display name is not a thing all three platforms have.** Windows SCM has one, and
`service-manager` sets it to the service name with no way to override — the same class of gap
ADR-0020 found in that backend's restart policy, and closable the same way, with a post-install call
through the `windows-service` crate that is already a Windows dependency. systemd has `Description=`,
which the crate fills with the unit name; changing it means supplying the entire unit text through
`ServiceInstallCtx::contents` and owning a template per platform — precisely the "hand-write the
three backends" alternative ADR-0010 weighed and rejected. launchd has no display name at all; a job
is its `Label`.

## Decision

We will register the service as **`opamp-fleet-client`** on systemd, launchd, and the Windows SCM
alike, and give it the display name **`OpAMP Fleet Client`** wherever the platform has one.

1. **The label is a single token, so every backend renders it the same.** `label()` builds
   `ServiceLabel { qualifier: None, organization: None, application }` rather than parsing a
   dotted string. The unit is `opamp-fleet-client.service`, the launchd job and its plist are
   `opamp-fleet-client`, and the SCM service is `opamp-fleet-client`. One name in one runbook.

2. **The default instance carries the bare name; any other appends its own.** `--instance prod`
   registers `opamp-fleet-client-prod`. ADR-0010 says most hosts run exactly one instance, and that
   host should show the product's name and nothing else; the suffix appears only where an operator
   deliberately asked for a second Client. A hyphen rather than a dot, because a dot would split the
   label again and undo point 1 — and the instance-name grammar (lowercase, digits, `-`) already
   guarantees the result is a legal unit name, launchd label, and SCM name.

3. **The display name is `OpAMP Fleet Client`**, and `OpAMP Fleet Client (prod)` for a named
   instance. It is set where it exists:
   - **Windows:** after `install`, with `sc.exe config <name> displayname= "…"` — in the same
     module (`windows_config`) that already closes this backend's restart-policy gap. Deliberately
     *not* the `windows-service` crate's `Service::change_config`: it maps onto
     `ChangeServiceConfigW` with every field taken from a `ServiceInfo`, so setting one field means
     restating the executable path, the launch arguments, the start type and the account exactly as
     registered — and one of them wrong rewrites the registration instead of the name. `sc config`
     changes the field it is given and leaves the rest alone, and it is also how the service was
     created, since the backend is the `sc.exe` wrapper. This makes ADR-0010's promise of a readable
     name in the services list true for the first time; it had never been implemented.
   - **systemd:** `Description=` is whatever the crate writes, which is the unit name — now
     `opamp-fleet-client`, the product's name rather than a mangled label. Making it the prose form
     would mean owning the unit template, and this ADR does not take that step.
   - **launchd:** nothing to set. A job is its label; there is no second name to give it.

4. **This renames an installed service, and there is no migration.** A service registered under the
   old label is not found under the new one. `service uninstall` with the *old* binary, then
   `service install` with the new one. Nothing has been released, so this is a developer-machine
   chore rather than a fleet operation — and it is the last moment that is true, for the same reason
   ADR-0028 gives about names in general.

## Alternatives considered

- **Keep reverse-DNS on launchd and the plain name elsewhere** — Apple's convention for a
  `LaunchDaemon` label is reverse-DNS, and following it is why the label looked like this at all.
  Rejected because it is the status quo restated: the operator still has two names, and the one
  advantage — matching a convention — is worth less than a name that is the same everywhere. The
  convention is a recommendation about collision avoidance, and `opamp-fleet-client` does not collide.
- **Always append the instance (`opamp-fleet-client-default`)** — one rule instead of two shapes,
  and it puts a word in front of every operator that carries no information on the hosts that run one
  Client, which is nearly all of them.
- **Own the unit, plist, and SCM registration ourselves** to control every field, `Description=`
  included — the maximal-control option ADR-0010 rejected, with three code paths to maintain. It
  buys a prose description on systemd and nothing else this ADR wants.
- **Change nothing and document the three names** — what the manual should have done all along, and
  the reason this ADR exists: the names were never chosen, and documenting an accident makes it
  permanent.

## Sources / Prior art

- `service-manager` 0.11.0, `ServiceLabel::to_qualified_name` and `to_script_name` — the two
  renderings, and the single-token case where they agree; `systemd::make_service`, which fills
  `Description=` with the unit name; and the `sc` backend, which sets `displayname=` to the service
  name.
- `windows-service` 0.8.1, `Service::change_config` — the API that looks like the way to set a
  display name and is not, because it restates the whole registration; and `sc.exe config`, which
  changes one field.
- What comparable agents register: `elastic-agent`, `datadog-agent`, `vector`, `telegraf` — a plain
  product name on every platform, not a reverse-DNS label, and a prose display name on Windows.
- [ADR-0020](0020-client-self-update.md)'s Windows section — the precedent for reaching past this
  crate with `windows-service` when a backend cannot express what the decision requires.

## Consequences

- **Positive:** one name to know, to document, and to type — `systemctl status opamp-fleet-client`,
  `launchctl list opamp-fleet-client`, `sc query opamp-fleet-client`. The Windows services list gets
  the readable name ADR-0010 promised and never delivered. The name matches the binary, the artifact,
  and the Agent in the fleet view, finishing what ADR-0028 started.
- **Negative / trade-offs:** a `LaunchDaemon` label that is not reverse-DNS departs from Apple's
  convention — accepted deliberately, and noted here so nobody re-derives it as a mistake. Every
  developer machine with an installed service must uninstall and reinstall. systemd's `Description=`
  stays a name rather than a sentence until someone owns the unit template. Two label shapes exist
  (bare, and suffixed for a named instance) instead of one.
- **Follow-ups:** the manual documents no unit name at all today and should carry the per-platform
  table this decision collapses into one row; `service install` prints the label and should print the
  name the platform will actually show. Whether to own the unit and plist text — for `Description=`,
  and for the systemd hardening directives a reviewer will eventually ask about — is a separate
  decision, and a larger one.
