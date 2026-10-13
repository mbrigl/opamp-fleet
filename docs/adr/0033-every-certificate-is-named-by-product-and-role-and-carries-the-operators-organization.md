# ADR-0033: Every certificate is named by its product and its role, and carries the operator's organization for audit

- **Status:** 🟡 proposed
- **Date:** 2026-10-13
- **Deciders:** Markus Brigl
- **Applies to:** the subjects of every certificate the project makes: the `pki` commands of `opamp-fleetctl` in `crates/fleet-tools/src/pki.rs`, the signing of host and bootstrap certificates in `crates/fleet-server/src/ca.rs` and the startup checks of `[client_ca]` and `[enrolment]` in `crates/fleet-server/src/config.rs`, `scripts/dev-pki.sh` and `scripts/seed_test_configs.sh`, and every document that shows a certificate's subject or names `--fleet`
- **Supersedes:** [ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md) clauses 5 and 6; [ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md) clause 7

## Context

The project makes six kinds of certificate: three CAs and a server certificate on the operator's
machine ([ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md)), bootstrap
certificates on the Server
([ADR-0032](0032-the-server-issues-bootstrap-certificates-under-a-bootstrap-ca-of-one-year.md)),
and host certificates the Server signs from a CSR
([ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
clause 9). Their subjects follow three rules today: the CAs are `CN=<fleet> server CA`, `client CA`
and `bootstrap CA` (ADR-0025 clause 5), a server certificate is named by its first `--name`
(ADR-0025 clause 6), and a host certificate keeps the subject its CSR asks for, the Client's
`service.instance.name` (ADR-0019 clause 7).

The forces:

- **An audit reads certificates, not configuration.** An auditor shown a certificate from a
  host, a Server or a trust store asks who issued it and what it is for. Today the answer is a
  fleet slug (`opamp-fleet`) and a role in lower case, with no organization anywhere in the chain.
  An operator who has to name the organization behind a certificate cannot put it there.
- **A certificate's role is not readable from its subject.** A server certificate reads like a
  host name, a host certificate like an Agent's name, a bootstrap certificate like whatever the
  operator typed. Which of the six a given PEM file is shows only in its extensions and its issuer.
- **Nothing reads a subject to decide anything.** A TLS client checks a server certificate's
  names against its subject alternative names and never against the common name (RFC 9525
  section 2), and `rustls` reads no common name at all. The Server tells an enrolment connection from
  a member's by its issuer (ADR-0019 clause 19), names a host by the SAN URI it puts in
  (ADR-0019 clause 7), and decides a renewal by its proof (ADR-0019 clause 27). The subject is
  free to say what an auditor needs.
- **A CSR's subject is the Client's to choose.** Every subject the Server copies from a CSR is
  self-asserted; one the Server sets is not.
- **A product is renamed when it is rebuilt.** A build may carry another product's name
  ([ADR-0031](0031-the-builds-product-variables-are-named-for-the-product.md)), and the name an
  operator wants in their certificates is that of their own installation, which no build constant
  knows.

This serves **G-17** (the connection is secured and the Agent is identified) and **Q-1** (secure
by default).

## Decision

We will name every certificate the project makes `<product> <Role> CA` or
`<product> <Role> Certificate`, with the product given by the operator, and let the operator add
an organization (O) and an organizational unit (OU) to every CA and server certificate.

1. **The product is a flag, with a default.** `opamp-fleetctl pki init` and `pki bootstrap-ca`
   take `--product <name>` (default `OpAMP Fleet`) in place of `--fleet`: letters, digits, space,
   `.` and `-`, 1 to 40 characters, no leading or trailing space. `--fleet` is an unknown option.
   No build constant enters a certificate.

2. **Restates ADR-0025 clause 5. The CAs are told apart by role and sign no CA.** The three CAs
   are `CN=<product> Server CA`, `CN=<product> Client CA` and `CN=<product> Bootstrap CA`, so
   the bootstrap CA never shares a subject with the client CA (ADR-0019 clause 19). Each CA
   certificate is `CA:TRUE` with a path length of 0, `keyCertSign` and `cRLSign`.

3. **The operator names an organization for audit.** `init` and `bootstrap-ca` take
   `--organization` (O) and `--organizational-unit` (OU), each optional, each 1 to 64 characters
   of UTF-8 without control characters (the upper bound of RFC 5280 Appendix A.1). `init` writes
   them into all three CAs and the server certificate. Attributes are encoded as `UTF8String`, in
   the order O, OU, CN. No other attribute is written.

4. **Restates ADR-0025 clause 6. A server certificate names what is dialled in its SAN and its
   role in its subject.** `--name` is given at least once and takes a DNS name or an IP address;
   an empty value, a wildcard, or a name that is neither is refused. Each becomes a subject
   alternative name, and `IP:127.0.0.1` and `IP:::1` are always added, so the Operator plane on
   its loopback default is reachable through a tunnel with the same certificate. The certificate
   is `CA:FALSE`, `digitalSignature`, `serverAuth`. Its common name is derived from the server CA's
   (clause 6). `pki server-cert` takes O and OU from the server CA, and `--organization` and
   `--organizational-unit` replace either, so a Gateway run by another unit says so.

5. **Restates ADR-0019 clause 7. A certificate proves membership and its host, not an Agent's
   identity.** The Server does not require the subject to match `instance_uid`,
   `service.instance.name`, or anything else an Agent reports. The request may dictate neither the
   certificate's subject nor its powers: the Server sets the subject to the common name of clause 6
   and nothing else, signs a client-authentication leaf only — `CA:FALSE`, `digitalSignature`,
   `clientAuth` — drops every requested SAN, and puts in one of its own, the URI
   `urn:opamp-fleet:host:<id>`. The CSR's subject is still listed with a pending enrolment
   (ADR-0019 clause 22) and still searched for claims (ADR-0019 clause 29). The host is minted when
   an operator approves an enrolment and carried on by every renewal (ADR-0019 clause 27); a
   certificate an operator provisioned without one is given a host derived from its issuer and
   serial when it is first renewed. An `instance_uid` is spoken for by the host whose certificate
   first reported it: a connection presenting another host's certificate that reports for it is
   re-keyed through `AgentIdentification` to an identity of its own, and a CSR in that message may
   not claim the old one. A re-key an Agent asks for keeps its host. A host holds at most three
   valid certificates at a time and speaks for at most 256 `instance_uid`s. A Gateway carries other
   hosts' Agents, so an operator marks it as one — `PUT /api/v1/hosts/{host}/gateway` — and its
   certificate then speaks for any Agent; behind it, membership alone applies. `GET
   /api/v1/hosts` lists each host, whether it is a Gateway, what it speaks for and how many valid
   certificates it holds.

6. **A leaf's common name is its CA's, with `CA` turned into `Certificate`.** Whoever signs a
   leaf — `opamp-fleetctl` a server certificate, the Server a host or a bootstrap certificate —
   takes the common name of the CA that signs it and replaces a trailing ` CA` with
   ` Certificate`, or appends ` Certificate` when there is none. A fleet made by `init` therefore
   has `<product> Server Certificate`, `<product> Client Certificate` and
   `<product> Bootstrap Certificate`, and a fleet whose CAs were made elsewhere gets a name from
   them without a setting of its own. The Server refuses to start when the name derived from the
   client CA or the bootstrap CA would exceed 64 characters.

7. **A bootstrap certificate's organization is the operator's choice per issue.** The common name
   of a bootstrap certificate is the one of clause 6. Its O and OU are what the issuing request
   names (ADR-0032 clause 2), typically the customer and the site, and not the bootstrap CA's; the
   organization behind it is read from its issuer. A host certificate carries no O or OU: its
   issuer names the organization, and its subject stays free for what a later decision may carry
   over from the bootstrap certificate.

8. **A development certificate says so.** `scripts/dev-pki.sh` makes its set with
   `--product "OpAMP Fleet Dev" --organization "Development"`, and
   `scripts/seed_test_configs.sh --enrol` issues its bootstrap certificate with the organization
   `Development`. Every certificate of a development set therefore reads `OpAMP Fleet Dev …` and
   `O=Development`, and one that turns up outside a developer's machine is recognized as such.

**Out of scope:**

- Renaming certificates that exist. A fleet made under ADR-0025 keeps its CAs, and its Server
  derives leaf names from them by clause 6 (`opamp-fleet client CA` gives
  `opamp-fleet client Certificate`).
- Attributes other than O, OU and CN, such as C, ST, L or an e-mail address.
- Carrying the bootstrap certificate's subject into the host certificate.
- An organization in a host certificate's subject.

## Alternatives considered

- **The product name from the build (`PRODUCT_TITLE`, ADR-0031).** It needs no flag, but it names
  the Client product (`OpAMP Fleet Agent`), lives only in `crates/fleet-agent`, and is the same for
  every installation of one build. An operator's certificates name their installation.
- **Keep `--fleet` and add only O and OU.** The smallest change, but it leaves the lower-case slug
  as the name an auditor reads and every leaf without a role in its subject.
- **A configuration file for issuer and subject data.** It holds everything in one place, but
  three values are set at most once a year, and a file has to be versioned, validated and
  documented. An operator with richer requirements makes the certificates in their own PKI, which
  the Server accepts (ADR-0032 clause 12).
- **Country, state and locality as well.** They describe where an organization sits, which an
  audit of a private fleet does not ask, and `country` needs an encoding of its own
  (`PrintableString`). O and OU name who is responsible.
- **Keep the CSR's subject in the host certificate.** It shows the Agent's name in the register,
  but that name is self-asserted, and the register already lists the host and the `instance_uid`
  of each certificate (ADR-0019 clause 33).
- **O and OU of the client CA in every host certificate.** It repeats the issuer in the subject
  and takes the place a customer would need if the bootstrap certificate's subject is ever carried
  over.
- **A setting for the leaf names in `server.toml`.** It lets an operator name leaves freely, but
  it is a second source for what the CA's name already says, and the two can drift.
- **No decision.** Certificates keep a slug and no organization, and an auditor reads the
  installation's identity from configuration instead of from what is presented.

## Sources / Prior art

- RFC 5280 sections 4.1.2.4, 4.1.2.6 and Appendix A.1: the issuer and subject, the attribute types, and
  the upper bound of 64 characters for organization, organizational unit and common name.
  <https://www.rfc-editor.org/rfc/rfc5280>
- RFC 9525 section 2: the common name must not be used to identify a service.
  <https://www.rfc-editor.org/rfc/rfc9525>
- CA/Browser Forum Baseline Requirements section 7.1.4: a publicly trusted server certificate's common
  name, when present, repeats one of its SANs. The rule binds public CAs only; the server CA here
  is private. <https://cabforum.org/working-groups/server/baseline-requirements/>
- Kubernetes PKI: the CAs are named for their role (`kubernetes`, `etcd-ca`, `front-proxy-ca`),
  and the kubelet's client signer permits one subject only, `O=system:nodes`,
  `CN=system:node:<name>`, whatever the CSR would like.
  <https://kubernetes.io/docs/setup/best-practices/certificates/>,
  <https://kubernetes.io/docs/reference/access-authn-authz/certificate-signing-requests/>
- [ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
  clauses 7, 19, 22, 27, 29 and 33, [ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md)
  clauses 5 and 6, [ADR-0031](0031-the-builds-product-variables-are-named-for-the-product.md).

## Consequences

- Positive: every certificate says what it is for in plain words, and each CA and server
  certificate names the organization behind it, so an audit reads both from the certificate
  presented (G-17).
- Positive: a host certificate's subject is the Server's alone, and no self-asserted text from a
  CSR reaches a signed certificate (Q-1).
- Positive: a fleet with CAs from its own PKI gets leaf names that match them without a setting.
- Negative / trade-offs: every host certificate of a fleet has the same subject. A host is told
  apart by its SAN, its serial and the register (ADR-0019 clause 33), not by its subject; an
  operator who read the Agent's name from a certificate reads it from `GET /api/v1/hosts` now.
- Negative / trade-offs: a server certificate's common name is not one of its names, so it could
  not be issued by a public CA in this form. It never is: the server CA is the fleet's own.
- Positive: a development certificate is told from a production one by its subject alone.
- Negative / trade-offs: `--fleet` disappears from `opamp-fleetctl`, and scripts that pass it
  fail until they pass `--product`.
- Follow-ups: carrying the bootstrap certificate's O and OU into the host certificate on approval;
  showing the issuer's O and OU wherever the Server lists certificates.

## Enforcement

Tests that will carry `Verifies: ADR-0033`:

- `pki init --product "Acme Fleet" --organization "Acme AG" --organizational-unit "IT"` writes
  CAs whose subjects are `O=Acme AG, OU=IT, CN=Acme Fleet Server CA`, `… Client CA` and
  `… Bootstrap CA`, and a server certificate `O=Acme AG, OU=IT, CN=Acme Fleet Server Certificate`
  with every `--name` and the loopback as SANs. Without the flags the CAs are
  `CN=OpAMP Fleet … CA` and carry no O or OU. `--fleet` is an unknown option, and a product or an
  organization outside its grammar is refused (clauses 1 to 4).
- `pki server-cert` copies O and OU from the server CA, and `--organizational-unit` replaces OU
  alone (clause 4).
- A host certificate the Server signs from a CSR with `CN=evil, O=Other` has the subject
  `CN=<product> Client Certificate` and nothing else, and a bootstrap certificate issued with
  `organization` and `organizational_unit` has those and `CN=<product> Bootstrap Certificate`
  (clauses 5 to 7).
- Under a client CA named `CN=Corp Root`, a host certificate is `CN=Corp Root Certificate`; under
  a client CA whose derived name exceeds 64 characters the Server refuses to start (clause 6).

**Not mechanically decidable:** that `scripts/dev-pki.sh` and `scripts/seed_test_configs.sh` pass
the development names of clause 8 is held by review; no test runs either script.
