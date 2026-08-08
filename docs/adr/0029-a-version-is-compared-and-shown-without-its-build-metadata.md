# ADR-0029: A version is compared and shown without its build metadata — the commit is provenance, not identity

- **Status:** 🟡 proposed
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes two rules, and nothing else in the ADRs that carry them:
[ADR-0020](0020-client-self-update.md)'s probe comparison — the staged binary had to report the
offered version *character for character* — and
[ADR-0025](0025-release-pipeline-and-artifacts.md)'s consequence of it, that `?version=` "must
therefore be the *full* baked string". How a version is computed and baked
([ADR-0009](0009-version-derivation-and-baking.md), [ADR-0026](0026-version-from-cargo-toml.md)) is
untouched: the string keeps its shape, the Client keeps reporting all of it, and every file that
records one keeps recording all of it.

## Context

The rule met its first operator and lost. A Client package was uploaded as `0.1.1`, the binary in it
reports `0.1.1+799e36a`, and the install never happened. The store still holds the fingerprint of the
attempt to fix it by hand:

```json
{ "version": "0.1.1", "previous": { "version": "0.1.1 799e36a" } }
```

That space is not a typo. `+` in a URL query string decodes to a space, so
`?version=0.1.1+799e36a` — the obvious way to type the full string — arrives as
`0.1.1 799e36a` and can never match anything. The correct spelling is `?version=0.1.1%2B799e36a`,
which is not a thing an operator should have to know to ship a release.

So the requirement as written is unusable in the one place it is used. But the deeper problem is that
**one string is carrying three different jobs**:

| Job | What it needs | What it gets today |
|---|---|---|
| *Which release is this?* | `MAJOR.MINOR.PATCH` | `0.1.1+799e36a` |
| *Which build is on that host?* | the commit, the `-dev` marker | the same string |
| *What does an operator read in a table?* | something scannable | `0.1.1-dev+799e36a` in a column headed "Version" |

The codebase already resolved this tension once, in the other direction. ADR-0010's version
directories are `opamp-client-<MAJOR.MINOR.PATCH>-<hash>` — deliberately "never the pre-release" —
because a *directory* names a version of a component while the manifest inside it records the build.
The identity is the base there, and the provenance sits beside it. This ADR applies the same split to
the two places that still conflate them.

One thing the probe is **not** for, which is what makes relaxing it safe: proving *which bytes*
arrived. That is already settled before the probe runs — ADR-0015 verifies the artifact's content
hash on every download, and its Ed25519 signature when a key is configured. The probe answers two
narrower questions ADR-0020 states plainly: *does this binary run at all on this host*, and *is it
this program rather than something else offered under the same name*. Neither needs a commit hash.

## Decision

We will treat a version's **build metadata as provenance** — recorded and reported everywhere,
compared nowhere, and shown only where someone asked for detail.

1. **One parser, in the shared crate.** A small helper in `opamp` splits a version string into its
   `MAJOR.MINOR.PATCH` base, an optional pre-release, and optional build metadata. It lives there
   rather than in either end because the Client writes these strings and the Server displays them,
   and ADR-0005 put the shared crate between them precisely so the two cannot drift.

2. **The self-update probe compares everything except the build metadata.** `0.1.1` offered and
   `0.1.1+799e36a` reported is a match; `0.1.1` offered and `0.1.1-dev+799e36a` reported is **not**.
   Dropping `+<hash>` is what fixes the trap, because the hash is the part an operator cannot type
   and does not know at upload time. Keeping the pre-release is what preserves the distinction
   ADR-0009 created it for: a `-dev` build is a build heading for a release and is not that release,
   and this probe is the last gate that can say so before a fleet installs one.

3. **An offered version that has no parseable base fails the install**, naming the value. Fail
   closed: a package version is free-form by the API's own contract, and one that is not a version
   cannot be compared to a version. This is the Client's own package, where the shape is ours.

4. **`?version=` takes the release number.** `0.1.1` is the expected spelling; the full string still
   works, since its base and pre-release are the same. Nothing has to be percent-encoded.

5. **The Server shows the base and the pre-release, and keeps the rest.** `AgentView.service_version`
   becomes `MAJOR.MINOR.PATCH[-prerelease]` — what belongs in a column headed "Version" — and the
   complete string as reported moves to a new `service_build` field beside it. The bundled UI shows
   the former in its Version column and the latter on hover, and searches both.

6. **The Client keeps reporting the full string.** `service.version` on the wire is unchanged;
   provenance has to reach the Server for anyone to answer "which build is on that host". What
   changes is what the Server puts in a column, not what an Agent says.

7. **Everything written to disk keeps the full string** — the version directory's `manifest.toml`,
   the self-update marker, `--version` output. Those are the internal record this decision relies on
   existing.

## Alternatives considered

- **Keep the exact comparison and document the encoding** (`%2B`) — the honest reading of the status
  quo, and it treats an unusable interface as a training problem. The operator who types the release
  number is the one behaving reasonably.
- **Compare the base only, dropping the pre-release too** — the literal form of the request that
  prompted this, and one step further than the problem needs. It would let an offer of `0.1.1` be
  satisfied by a `0.1.1-dev` build, which is exactly the confusion ADR-0009 introduced `-dev` to
  prevent, at the only gate that inspects a binary before a fleet runs it. If the pre-release should
  be ignored as well, that is a one-line change to the comparison and it belongs in this ADR's
  decision rather than in a later surprise.
- **Have the Client report only the base to the Server** — the smallest change to the display
  problem, and it throws away the answer to "which build is on that host", which is the question a
  fleet exists to answer.
- **Leave `service_version` holding the full string and add a base field beside it** — additive, so
  no API consumer breaks. Rejected for point 5 because it leaves a field called `service_version`
  holding something nobody would call a version, and because the REST API is at `v1` with no external
  consumer: the moment to give the field its right meaning is now, not after one exists. This is the
  reversible half of the decision, and the CHANGELOG carries it as breaking either way.
- **Compare with a full SemVer precedence implementation** (a `semver` dependency) — the correct
  answer if versions were ever *ordered* here. They are not: the probe asks "is this the one I was
  offered", an equality question over a string this project itself produces.

## Sources / Prior art

- The failure that prompted this, in `fleet-packages/opamp-fleet-client.json`: a package registered
  as `0.1.1` against a binary reporting `0.1.1+799e36a`, and a previous attempt recorded as
  `0.1.1 799e36a` — a `+` that a query string turned into a space.
- [SemVer 2.0.0](https://semver.org/), rule 10: "Build metadata MUST be ignored when determining version
  precedence… two versions that differ only in the build metadata have the same precedence." The
  specification the version strings already follow says the hash is not part of the identity.
- [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986#section-2.2) and the
  `application/x-www-form-urlencoded` convention behind `+` meaning a space in a query string — why
  the full string cannot be typed into a URL unencoded.
- [ADR-0010](0010-client-os-service-and-cli.md)'s version-directory naming — the same base/provenance
  split, already decided once in this codebase.

## Consequences

- **Positive:** a release is uploaded under the number it is called, with no encoding lore. The fleet
  table becomes scannable and still answers the provenance question on hover. The probe keeps the two
  checks it exists for and loses the one it never needed. `-dev` gains a place where it is actually
  enforced rather than merely displayed.
- **Negative / trade-offs:** `AgentView.service_version` changes meaning inside `/api/v1` — a
  breaking change for any consumer, of which there is currently one, the bundled UI, changed in the
  same commit. Two versions that differ only by commit are now indistinguishable to the probe, so
  re-offering a rebuilt artifact of the same release is accepted rather than refused; the content
  hash is what distinguishes those bytes, and it already did.
- **Follow-ups:** the operator manual's self-update walkthrough and the release notes template both
  tell an operator to type the full version, and are corrected with this change. Whether the *package*
  version of a Managed Process (free-form by contract, an upstream project's own numbering) should be
  parsed at all is deliberately left alone here.
