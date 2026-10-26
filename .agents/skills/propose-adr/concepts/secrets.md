# Secrets

Where a project keeps its credentials and how a leak is caught, beyond the baseline every project
has: no secret in a tracked file, and rotation first after a leak
([`AGENTS.md` §7](../../../../AGENTS.md#7-secrets)). The baseline holds secrets in environment
variables, gitignored `.env*` files, and GitHub Actions secrets; `SECURITY.md` says what the
repository's configuration does and where it stops.

## Signals

- *Threats & Forbidden Actions* in the specification names credentials as an asset: API keys,
  database passwords, signing keys.
- More than one environment needs the same secret, or more than one person.
- A credential has a shape GitHub's secret scanning does not know.

## Fits when

- **Baseline only**: one developer, few credentials, all of them in GitHub's scanning patterns.
- **A scanner in CI** (gitleaks, TruffleHog): credentials of the project's own shape, or a
  repository where push protection cannot be enabled; the scanner fails the pull request.
- **Encrypted files in the repository** (sops): configuration and its secrets travel together
  and are versioned, decrypted with a key that is not in the repository.
- **A secret manager** (Vault, the cloud provider's): several environments, rotation on a
  schedule, access that is audited and revoked per person or service.

## Does not fit

- A scanner as the only measure: it finds a secret after it was written, and rotation is still
  the remedy.
- A regex scan written by hand in `scripts/`: false confidence, it misses most real leaks.
- A secret manager for a project with two credentials and one developer: an operation to run for
  nothing.

## The rule a test decides

With a scanner: its CI job fails on a finding, and its self-test plants a fake secret and expects
the failure. Without one, the rule is review and push protection, and the ADR says so.

## Sensors

The scanner the ADR names; GitHub secret scanning with push protection as a repository setting.

## Alternatives

The four options above are each other's alternatives and combine: the baseline always, a scanner
where shapes are the project's own, sops or a manager where secrets outgrow a `.env` file.

## Sources

- GitHub, *About secret scanning* —
  <https://docs.github.com/en/code-security/secret-scanning/introduction/about-secret-scanning>:
  scanning and push protection as repository settings.
- GitHub, *Remediating a leaked secret* —
  <https://docs.github.com/en/code-security/tutorials/remediate-leaked-secrets/remediating-a-leaked-secret>:
  revoking with the provider is the most important step; removing it from the code is not enough.
- gitleaks — <https://github.com/gitleaks/gitleaks>.
- TruffleHog — <https://github.com/trufflesecurity/trufflehog>.
- sops — <https://github.com/getsops/sops>: encrypted values in versioned files.
