# ADR-0072: The Windows artifact is verified by its publisher — the Authenticode signature, pinned to Icinga GmbH

- **Status:** 🟡 proposed
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

[ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) binds a rule this project
does not bend: **nothing is repacked before it is verified.** For the Linux artifacts that is
answered by the repository index, whose `SHA256` field states a digest per file.

The Windows artifact has no such index. `packages.icinga.com/windows/` is a directory listing of
`.msi` files with **no digest sidecars at all** — `.sha256`, `.sha256sum`, `.asc` and `.md5` all
answer `404`. What the MSI does carry is an **Authenticode signature**, and it checks out where it
matters:

```
Signer: /C=DE/ST=Bayern/L=Nuernberg/O=Icinga GmbH/CN=Icinga GmbH
Issuer: GlobalSign GCC R45 CodeSigning CA 2020
Number of verified signatures: 1
Timestamp Server Signature CRL verification: ok
Error: unable to get local issuer certificate
```

The signature covers the file's contents and is timestamped; only the chain to a root fails, because
a Linux CA bundle carries web PKI roots and not the code-signing roots Windows trusts. That is a
property of the build host, not of the artifact.

Two things make this worth deciding rather than improvising. A signature is **not the weaker
substitute** for a digest here: a digest published beside a file on the same server is only as
trustworthy as that server, while a signature is bound to a key the server does not hold — so an
attacker who controls the mirror can rewrite both file and digest, and cannot forge the signature.
And the decision generalises: any future artifact published without an index — a macOS package, a
vendor's direct download — meets the same question.

## Decision

We will verify the Windows MSI by its **Authenticode signature, pinned to the publisher**, and
repack it only when that verification passes.

Bound by this decision:

- **The check is two conditions, both required**: the embedded signature verifies against the file's
  contents, and the signer's subject names the expected publisher — `O=Icinga GmbH` for Icinga's
  MSI. A file that is unsigned, altered, or signed by somebody else is refused by name, and nothing
  is unpacked.
- **The chain to a root is reported, not required.** A Linux build host has no Authenticode root
  store, and carrying one inside this tool would be key management nobody asked it to do. What the
  refusal cannot claim, it does not claim: the tool says the signature is valid and whose it is, and
  says that the issuing chain was not validated locally.
- **The expected publisher is part of the agent's own definition**, beside its repository — not a
  flag. A publisher an operator can pass on the command line is a check that argues with itself.
- **`osslsigncode` is the verifier**, shelled out to as the extraction helpers are (ADR-0070), and
  refused by name when absent. The Dev Container gains it.
- **This says nothing about whether the artifact runs.** Whether a repacked Windows tree relocates
  without the MSI's own product registration is the open question ADR-0070 already records, and it
  needs a Windows host. Verification is about the bytes being the vendor's; the recipe still states
  Windows as unproven until someone runs it there.

## Alternatives considered

- **Require an operator-supplied `--sha256`.** What ADR-0070 first imagined for the Linux packages.
  Rejected here: it moves the trust decision to whoever types the command, with a digest they took
  from the same page as the file — and it makes the ordinary path a manual one, which is how
  verification comes to be skipped.
- **Trust the TLS connection to `packages.icinga.com` and repack.** Rejected outright: it would put
  the artifact's integrity in the hands of whoever serves that path, which is exactly what the
  verification rule exists to avoid.
- **Carry the GlobalSign code-signing root and validate the full chain.** Stronger on paper. Rejected
  for now: it pins this tool to one CA's roots and their rotation, and the publisher check already
  binds the artifact to a key an attacker on the mirror does not have. Revisit if a deployment needs
  a chain-verified provenance claim.
- **Verify on a Windows host instead**, where the chain validates. Rejected as a requirement: the
  repack itself runs on Linux (the MSI extracts there), and needing a second platform to check a
  download would make the Windows artifact harder to produce than to trust.
- **Skip Windows until it is proven to run.** Tempting, and it confuses two questions. Whether the
  bytes are the vendor's is answerable today; whether the tree relocates is not, and is recorded as
  open.

## Sources / Prior art

- Measured against `Icinga2-v2.16.4-x86_64.msi` (2026-08-17): no digest sidecar published; the
  embedded signature verifies with a timestamp, signer `O=Icinga GmbH`, issuer GlobalSign GCC R45
  CodeSigning CA 2020; chain validation fails on a Linux CA bundle only.
- [`osslsigncode`](https://github.com/mtrojnar/osslsigncode) — the OpenSSL-based Authenticode
  verifier, packaged by Debian; the same shell-out pattern ADR-0070 established for `dpkg-deb` and
  `msiextract`.
- [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) — the rule this
  implements for a source that has no index, and the open question about Windows relocation.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), [ADR-0018](0018-packages-imported-from-a-url.md)
  — the fleet's own model, where a package's content hash and Ed25519 signature protect what a
  Client installs; this decides the *other* end, what the operator's tool is willing to repack.

## Consequences

- Positive: the Windows artifact is bound to Icinga's signing key rather than to a mirror's honesty
  — a stronger claim than the Linux path's digest, from a source that publishes less.
- Positive: the question generalises, so the next artifact without an index has an answer already.
- Negative / trade-offs: another shelled-out helper, and one more thing a build host must carry.
- Negative / trade-offs: the chain is unvalidated locally, so what is proved is "signed by a key
  whose certificate says Icinga GmbH" rather than "signed by a certificate a trusted root vouches
  for today". Stated in the output rather than glossed.
- Negative / trade-offs: a publisher rename, or a signing certificate issued to a differently spelled
  subject, breaks the build until this project is updated. Accepted: a pin that never fails is not a
  pin.
- Follow-ups: whether the repacked Windows tree runs relocated — the open question of ADR-0070,
  needing a Windows host; and chain validation against a carried root, if provenance ever has to be
  provable rather than merely bound.
