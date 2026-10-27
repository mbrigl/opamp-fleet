# Action references

How the workflows under `.github/workflows/` reference the GitHub Actions they run. Every `uses:`
line runs third-party code with the repository's token, so the form of the reference decides how
current that code stays and how far a compromise of its repository reaches. The template's
workflows use first-party actions by major version tag, with Dependabot configured.

## Signals

- A workflow adds an action outside `actions/*`.
- The specification's threats name the CI token, the release, or the published artifact.
- A reviewer or an agent "fixes" a tag into a SHA, or a SHA into a tag.

## Fits when

- **Major version tag** (`owner/action@vN`) with Dependabot raising the next major: first-party
  actions and few maintainers; fixes within a major arrive without a pull request.
- **Full commit SHA** with a version comment, kept current by Dependabot, which
  updates the pin with its comment, or by pinact: third-party actions, a release pipeline, or a
  token with write access; every change of the code that runs is a reviewed diff.

## Does not fit

- A major tag for an action whose repository the project has no reason to trust: a repointed tag
  runs in CI before anyone sees a diff, which is what happened to `tj-actions/changed-files`.
- SHA pins without a tool that updates them: they freeze security fixes out as well.
- A full version tag (`@v7.0.1`): as mutable as the major tag and as frozen as a SHA.

## The rule a test decides

A check over every uncommented `uses:` line that fails on the form the ADR excludes, and on a
missing `github-actions` entry in `.github/dependabot.yml` where Dependabot is part of the
decision. Either way the check also covers `docker://` images, which carry an explicit tag,
and exempts local actions (`./…`). `scripts/sensor-action-refs.sh --require major-tag` or
`--require sha` is that check; the project runs it with the flag in `scripts/check-all.sh` and
in the `sensors` job, beside a comment that cites the ADR with a `Verifies:` marker. zizmor's
`unpinned-uses` audit decides the SHA variant by default; since v1.20.0 it flags tag references
to `actions/*` too, so the tag variant needs a zizmor configuration that allows them.

## Sensors

`scripts/sensor-action-refs.sh`, which ships with the template: without a flag it reports mixed
forms, a branch or a missing ref, and an untagged image, and fails nothing. With `--require` it
turns the decision into a gate; zizmor (`unpinned-uses` and the other workflow audits) goes
further.

## Alternatives

The two forms above are each other's alternative. A mixed rule, tags for `actions/*` and SHAs for
everything else, is a third; its check needs the list of trusted owners.

## Sources

- GitHub, *Security hardening for GitHub Actions* —
  <https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions>:
  a full SHA is the only immutable reference; a tag can be moved.
- GitHub Advisory GHSA-mrrh-fwg8-r2c3 / CVE-2025-30066 —
  <https://github.com/advisories/GHSA-mrrh-fwg8-r2c3>: the tags of `tj-actions/changed-files`
  repointed to a commit that printed the runner's secrets.
- GitHub, *Keeping your actions up to date with Dependabot* —
  <https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot>.
- zizmor, *ref-version-mismatch* — <https://docs.zizmor.sh/audits/>: Dependabot as a tool that
  refreshes the version comment of a hash-pinned action.
- pinact — <https://github.com/suzuki-shunsuke/pinact>: pins actions to SHAs and updates them.
- zizmor — <https://github.com/zizmorcore/zizmor>; audits at <https://docs.zizmor.sh/audits/>.
