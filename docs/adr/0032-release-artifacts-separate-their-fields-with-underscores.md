# ADR-0032: A release artifact separates its four fields with `_` — `name_version_os_arch.7z`

- **Status:** 🟡 proposed
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **file-name separator** of [ADR-0025](0025-release-pipeline-and-artifacts.md)
clause 4, as amended by [ADR-0031](0031-per-platform-package-variants.md) clause 7. Everything else
both ADRs decide stands and is still binding: the `version/*` trigger, the five targets and their
runners, the `.7z` container written by this project's own packer, the version read once from a
built binary, what a release publishes — and, from ADR-0031, the platform vocabulary itself
(`linux`/`darwin`/`windows`, `amd64`/`arm64`), which two tokens name a platform, and that the file
name states exactly the pair an Agent reports. This decision changes what stands *between* the
fields, and nothing else.

## Context

A release asset is `opamp-fleet-client-0.1.1-linux-amd64.7z` today: four fields, one separator, and
**two of the four fields legitimately contain that separator**.

- The name does. ADR-0010's name grammar is `[a-z0-9-]`, and the product's own name spends it three
  times: `opamp-fleet-client`.
- The version does. ADR-0009 bakes a SemVer string and ADR-0029 puts the base version in the file
  name — but a base version may carry a prerelease (`0.1.2-dev`, which every build off a tag-less
  clone reports), and SemVer spells that with a hyphen.

So the name does not parse back. Everything that reads one has to *guess* where the fields are, and
this project has two such readers.

**The fleet view guesses.** Picking an artifact prefills the upload form's four fields
([`index.html:739`](../../crates/server/static/index.html#L739)): it splits on `-`, takes the last
two tokens as the platform, and then locates the version as "the first remaining token that begins
with a digit". That heuristic holds for our own artifacts and is wrong in general — an upstream
build called `otelcol-2-1.0.0-linux-amd64` has its name cut after `otelcol`. It is a guess with a
plausible-looking answer, which is the worst kind here: a wrong platform is a package no Agent is
ever offered (ADR-0031 point 3), and the operator sees a filled-in form rather than an error.

**The release notes guess too**, by handing the reader the answer up front: the published upload loop
([`release.yml:244`](../../.github/workflows/release.yml#L244)) strips the `<name>-<version>-` prefix
by pasting it in as a literal, then splits what is left. It works only because the loop is generated
by the job that already knows the version, so it cannot be the general instruction it looks like.

**The project has already solved this once, and the reasoning points at `_`.** The Server's on-disk
variant is `<name>@<os>-<arch>` precisely because of a grammar argument
([`packages.rs:1085`](../../crates/server/src/packages.rs#L1085)): "the ADR-0010 name grammar admits
neither `@` nor `_`, and a canonical platform token admits neither `@` nor `-`, so the parts of this
never run together." Apply the first half of that sentence to the release file name and the answer
is already there: a name cannot contain `_`, and neither can a SemVer version — `_` is not in
SemVer's alphabet for a prerelease or for build metadata. A separator the fields cannot contain is
what makes a file name parse back instead of being guessed at.

**And it is what this artifact's neighbourhood already looks like.** The fleet's most common managed
process ships as `otelcol_0.157.0_linux_amd64.tar.gz`; that is GoReleaser's default archive name,
`{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`, which most of the Go-ecosystem releases an
operator downloads are named by. Debian has separated a package's fields with `_` for the same reason
for decades — `hello_2.10-2_amd64.deb`, where the version keeps its hyphen and the fields do not lose
their boundaries. An operator who has ever unpacked a Collector release already knows how to read
this shape.

## Decision

We will name a release artifact **`<name>_<version>_<os>_<arch>.7z`** — the same four fields ADR-0025
and ADR-0031 decided, separated by `_` instead of `-`.

1. **The five artifacts of a release**, with `<version>` still the base version without the
   `+<hash>` build metadata (ADR-0025 clause 4, ADR-0029) and the platform tokens still ADR-0031's:

   | target | `<os>` | `<arch>` | artifact |
   |---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `linux` | `amd64` | `opamp-fleet-client_1.2.3_linux_amd64.7z` |
   | `aarch64-unknown-linux-gnu` | `linux` | `arm64` | `opamp-fleet-client_1.2.3_linux_arm64.7z` |
   | `aarch64-apple-darwin` | `darwin` | `arm64` | `opamp-fleet-client_1.2.3_darwin_arm64.7z` |
   | `x86_64-apple-darwin` | `darwin` | `amd64` | `opamp-fleet-client_1.2.3_darwin_amd64.7z` |
   | `x86_64-pc-windows-msvc` | `windows` | `amd64` | `opamp-fleet-client_1.2.3_windows_amd64.7z` |

2. **The name is read by splitting, not by guessing.** Neither the ADR-0010 name grammar nor a
   SemVer version admits `_`, so the four fields are the four `_`-separated fields of the stem. The
   fleet view's prefill and the release notes' upload loop both become that split, and neither needs
   to know the product's name or version in advance to find the platform.

3. **Read from the right, and rejoin a platform token that carries the separator.** ADR-0031 point 5
   lets a *canonical* platform token match `[a-z0-9_]{1,16}`, and its compatibility table knows
   `x86_64` — so a foreign artifact named `foo_1.0.0_linux_x86_64.tar.gz` splits into five fields,
   not four. The rule for that: take the arch from the last field, and if it is not a token this
   vocabulary knows, try the last *two* fields joined by `_` before giving up. Nothing this project
   publishes can hit it (`amd64`, `arm64`), and a name that still does not resolve fills nothing
   rather than filling a guess.

4. **Only the release file name changes.** A platform is still `<os>-<arch>` where it is a *tag*
   rather than a field of a file name: the Server's `<name>@<os>-<arch>.json`/`.bin`, the fleet
   view's platform column, the `linux-amd64` in prose. Those keep `-` for the mirror-image reason —
   a canonical token may contain `_` but never `-` — and the API keeps taking `os` and `arch` as two
   separate query parameters, so nothing there has to parse anything at all.

5. **Artifacts published before this keep their names.** Nothing is renamed and no release asset is
   rewritten: a checksum published against a URL stays true. Both shapes are therefore readable —
   the fleet view's prefill keeps accepting `-`, which it must anyway, because upstream artifacts an
   operator uploads are named by upstream and many of them use hyphens.

6. **The change is announced where operators look.** A glob or a script pinned to
   `…-linux-amd64.7z` stops matching, which is exactly the public-contract cost ADR-0025 named. The
   `CHANGELOG.md` entry and the release notes say so in the release that first carries the new shape.

## Alternatives considered

- **Keep `-` and specify the heuristic** — write down "the platform is the last two tokens, the
  version starts at the first token beginning with a digit" as the contract. Rejected: it is not a
  grammar, it is a lookahead that happens to work on our own five names. It cannot state what
  `otelcol-2-1.0.0-linux-amd64` means, and the ambiguity is structural — one separator cannot both
  occur inside a field and delimit it.
- **`name_version_os-arch.7z`** — `_` between the three parts and the platform kept as the Server's
  hyphenated tag. This is, honestly, the tighter grammar: it is unambiguous in *both* directions,
  because a name and a version cannot contain `_` and a platform token cannot contain `-`, so point
  3's rejoin rule would not be needed at all. Rejected because it is a fourth shape that nothing else
  in the world writes, against a convention (GoReleaser's default, the Collector's own releases,
  Debian) that operators already read fluently — and because the one case the rejoin rule covers is a
  foreign artifact, never ours.
- **A sidecar manifest per artifact** — publish the four fields as JSON beside the `.7z` and read
  them from there. Rejected: an operator downloads one file and hands that one file to a Server, so
  it must be self-describing; a second file that can be lost or skipped is a worse contract than a
  parseable name.
- **Rename the assets of releases already published** for one consistent history. Rejected for the
  reason in decision point 5: ADR-0025's public-contract argument cuts both ways — change the shape
  going forward, never rewrite what was published.

## Sources / Prior art

- [GoReleaser — Archives](https://goreleaser.com/customization/archive/): the default
  `name_template` is `{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`. This is the tool most
  Go-ecosystem projects release with, so it is the de-facto shape of the artifacts an operator of
  this fleet already downloads — including the one that matters most here.
- [OpenTelemetry — Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/):
  the Collector's own releases are `otelcol_<version>_linux_amd64.tar.gz`. The artifact this
  project's Supervisor most often delivers is already named the way this decision names ours, which
  is also why the fleet view's prefill gets *more* right, not less, by learning `_`.
- [Debian FAQ § Basics of the package management system](https://www.debian.org/doc/manuals/debian-faq/pkg-basics.en.html)
  and [dpkg-name(1)](https://www.man7.org/linux/man-pages/man1/dpkg-name.1.html):
  `<package>_<version>-<revision>_<architecture>.deb`, and `dpkg-name` renames files *into* that
  shape. The oldest large-scale instance of exactly this trade-off — a version that keeps its hyphens
  inside a field whose boundaries stay legible.
- [Semantic Versioning 2.0.0](https://semver.org/): the alphabet of a prerelease identifier and of
  build metadata is `[0-9A-Za-z-]` — a hyphen is *in* a version and an underscore cannot be, which is
  the whole grammatical basis of decision point 2.
- [ADR-0010](0010-client-os-service-and-cli.md) — the `[a-z0-9-]` name grammar, the other half of
  that basis, and the reason a name is three hyphen-separated words to begin with.
- [ADR-0031](0031-per-platform-package-variants.md) clause 5 and 7 — the platform vocabulary and the
  canonical-token grammar `[a-z0-9_]{1,16}`, which is both what makes the file name state a reported
  platform verbatim and the single ambiguity decision point 3 has to answer.
- [`packages.rs:1085`](../../crates/server/src/packages.rs#L1085) — this project's own prior art: the
  separator for the on-disk variant stem was already chosen by asking which characters the parts
  cannot contain. Same question, same method, different answer, because a file name's fields are the
  name and the version rather than two platform tokens.

## Consequences

- Positive: a release file name parses back into its four fields by splitting on one character, so
  the fleet view's prefill becomes correct rather than lucky, and the release notes can publish an
  upload loop that reads `os` and `arch` out of the file name without being told the name and version
  first. The shape matches what the Collector and most of the Go ecosystem publish, so an operator
  recognises it — and the prefill, accepting both separators, then serves upstream artifacts and ours
  with one code path.
- Negative / trade-offs: a platform pair is now spelled two ways in this project — `linux_amd64`
  between the fields of a release file name, `linux-amd64` as a tag on disk, in the API's answers and
  in the fleet view — which is one more thing to know, accepted because the two live in different
  grammars and each separator is the one its neighbours cannot contain. Artifacts published before
  this exist under the old shape forever, so both must stay readable; an operator's glob or download
  script pinned to `…-linux-amd64.7z` breaks and has to be updated once. The rejoin rule of point 3
  is a wart that the rejected `name_version_os-arch.7z` would not have needed.
- Follow-ups: none required. If a platform token this project *publishes* ever carries `_`, the
  right-to-left rejoin rule is the one place that has to be revisited; and if a future release ever
  ships something other than one binary per platform, the field set — not the separator — is what
  that decision would touch.
