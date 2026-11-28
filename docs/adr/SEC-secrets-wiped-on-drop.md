# SEC-secrets-wiped-on-drop: The application crates wipe the secrets they hold in memory when they drop them, with `zeroize`

- **Status:** 🟢 accepted
- **Date:** 2026-11-15
- **Deciders:** Markus Brigl
- **Applies to:** the `zeroize` crate in `Cargo.toml` and in the manifests of `crates/fleet-core/`, `crates/fleet-util/`, `crates/fleet-agent/`, `crates/fleet-server/` and `crates/fleet-tools/`, every value of those crates that holds a private key, a password, the S3 secret access key, a `bootstrap.p12` being read or the Supervisor Endpoint token (among them `BootstrapIdentity` and `pem_block` in `crates/fleet-util/src/identity.rs`, `open` in `crates/fleet-util/src/identity/pkcs12.rs`, `crates/fleet-agent/src/tls.rs`, `crates/fleet-agent/src/csr.rs`, `read_password_file` in `crates/fleet-util/src/password.rs`, the questionnaire in `crates/fleet-agent/src/config_init.rs`, the token in `crates/fleet-agent/src/supervisor/`, `crates/fleet-core/src/pkcs12/file.rs`, `crates/fleet-server/src/ca.rs`, `Password` in `crates/fleet-server/src/bootstrap.rs`, the Basic password in `crates/fleet-server/src/credentials.rs`, the Server's own TLS key, `read_secret_file` in `crates/fleet-server/src/config.rs`, the contents of `[database.postgres] password_file` in `crates/fleet-server/src/postgres/mod.rs` and of `[blobs.s3] secret_access_key_file` in `crates/fleet-server/src/s3.rs`, `[packages] archive_key` in `crates/fleet-agent/src/config.rs` and `crates/fleet-agent/src/config_file.rs`, the text of `supervisor.toml` where `crates/fleet-agent/src/reconfigure.rs` and `crates/fleet-agent/src/config_init.rs` rewrite it, the Gateway's listener key, and the password and key handling of `crates/fleet-tools/src/pki.rs` and `crates/fleet-tools/src/package.rs` with `package pack --archive-key`)

## Context

The [specification](../SPECIFICATION.md) puts security before convenience and holds that no
vulnerability can be ruled out (Strategy *Security before convenience*). Both ends hold secrets in
memory:

- the Client's private key as PEM, read from `client_key_file` or from the pending and issued key
  files under `state_dir`, and the key `csr.rs` reads to sign a renewal or a bootstrap proof;
- the bootstrap identity a `bootstrap.p12` opens to, held in `ClientConfig` until the Client is
  enrolled
  ([SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)),
  the file's bytes while the Client reads it, and its password from the questionnaire or a
  password file;
- the Server's client-CA and bootstrap-CA keys as PEM while it loads them, the DER of every key it
  seals into a PKCS#12 file, the bootstrap password of an issuing request, and the password of an
  Operator plane Basic login before it is verified;
- the PostgreSQL password and the S3 secret access key that `read_secret_file` reads from
  `[database.postgres] password_file` and `[blobs.s3] secret_access_key_file`
  ([SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md));
- the Supervisor Endpoint token ([CLT-modes-and-gateway](CLT-modes-and-gateway.md));
- the passwords and keys `opamp-fleetctl` reads and writes.

Freed memory keeps its bytes until it is reused. A core dump, a swapped page, a heap read past a
buffer, or a later allocation handed out uninitialised can reveal them. Overwriting a buffer by
hand does not hold: the compiler may remove a write to memory that is freed right after. `zeroize`
is the established crate for this. It writes through volatile operations behind a compiler fence.
The tree already builds it at 1.9.1 through `dialoguer`'s `password` feature, `der`, `pkcs8`,
`pkcs12`, `rustls` and `rustls-pki-types`, so naming it costs no new crate and no new licence.

What the crates around the secrets do, read in the locked sources:

- `rustls` 0.23.46 wipes its own traffic keys (`AeadKey`, `Tag`, the HPKE key) on drop. Its `ring`
  provider wraps the `PrivateKeyDer` it is handed in `Zeroizing` while it loads it. What it keeps
  after loading is a `ring` key.
- `rustls-pki-types` 1.15.1 implements `Zeroize` for `PrivateKeyDer` and its three forms, but not
  `Drop`: a key is wiped only when the code that holds it calls `zeroize` or wraps it.
- `ring` 0.17.14 wipes no key it holds; its sources say so.
- `rcgen` 0.14.10 implements `Zeroize` for `KeyPair` only behind its `zeroize` feature, which is
  off here, and never on drop. The `ring` key inside it stays unwiped either way.
- `p12-keystore` 0.4.1 wipes nothing. `pkcs12` 0.2.0-pre.0 wipes its derived keys and its copy of
  the password.
- `dialoguer` 0.12.0 holds the typed password in `Zeroizing` and returns a plain `String` clone.
- `tokio-postgres` 0.7.18 copies the password `Config::password` is handed into a `Vec<u8>` of its
  own. `object_store` 0.14.2 keeps the key `AmazonS3Builder::with_secret_access_key` is handed as
  a `String`. Neither names `zeroize`.

`opamp::tls::Identity` carries the key as PEM into every connection
([DEV-six-crates-tokio-axum §4](DEV-six-crates-tokio-axum.md)).
[DEV-six-crates-tokio-axum §3](DEV-six-crates-tokio-axum.md) lists the dependencies each feature
of `opamp` adds, and `zeroize` is not among them.

## Decision

We will make `zeroize` a direct dependency of the application crates and hold every secret their
own code keeps in a `Zeroizing` buffer. The buffer holds it from the moment it is read or made
until it is dropped. A secret here is any of these:

- a private key in any encoding;
- a password;
- the S3 secret access key;
- the bytes of a `bootstrap.p12` while the Client reads it;
- the Supervisor Endpoint token.

1. **One dependency, default features.** `[workspace.dependencies]` pins `zeroize` once
   ([DEV-six-crates-tokio-axum §11](DEV-six-crates-tokio-axum.md)), with its default `alloc`
   feature and no derive. `fleet-core`, `fleet-util`, `fleet-agent`, `fleet-server` and
   `fleet-tools` name it where they hold a secret. It pulls no TLS or crypto backend, so the rule
   of [SEC-tls13-bounded-input-chosen-planes §5](SEC-tls13-bounded-input-chosen-planes.md) on the
   TLS and crypto backend holds.
2. **`Zeroizing` is the whole mechanism.** A secret is a `Zeroizing<Vec<u8>>` or a
   `Zeroizing<String>`, or a struct field of that type. No type of this project's own wraps it. A
   function that hands a secret on returns it in that type. A borrow (`&[u8]`, `&str`) passes it to
   a function that only reads it.
3. **A copy is made only where a library takes ownership.** Where a crate demands an owned
   `String` or `Vec<u8>` of a secret, the code makes the copy at that call and nowhere earlier.
   While this project's code holds a crate's own type that implements `Zeroize`, such as
   `PrivateKeyDer`, it wraps that value in `Zeroizing`.
4. **What a library keeps is not wiped.** The following lie outside the reach of this project's
   code, and this decision makes no claim about them:
   - the keys `ring` holds inside `rcgen::KeyPair` and inside a rustls configuration;
   - the copies `reqwest` and `p12-keystore` make;
   - the PostgreSQL password `tokio_postgres::Config::password` copies;
   - the S3 secret access key `AmazonS3Builder::with_secret_access_key` keeps;
   - the token the Managed Process receives.

**Out of scope:**

- `opamp`. Wiping the key `opamp::tls::Identity` carries adds `zeroize` to both features of the
  publishable crate and changes the type of a public field. That changes
  [DEV-six-crates-tokio-axum §3](DEV-six-crates-tokio-axum.md), supersedes that ADR, and is its own
  decision.
- Locking secret pages against swap (`mlock`), and excluding them from core dumps.
- The ciphertext the Server keeps of a bootstrap certificate, which lies at rest in the Server's
  store anyway: under `data_dir` with the filesystem store, in the database with PostgreSQL.

## Alternatives considered

- **Leave memory as it is.** No dependency, but a secret outlives its use for as long as the
  allocator leaves the page alone. Rejected under *Security before convenience*.
- **Overwrite buffers by hand.** No dependency, but the compiler may remove a plain write to
  memory about to be freed. The volatile writes and the fence that prevent it are what `zeroize`
  already is. Rejected.
- **A `Secret` type of this project's own, or the `secrecy` crate.** It would also redact `Debug`
  and gate access behind `expose_secret`. It costs a second crate and a wrapper every call site
  unwraps, where `Zeroizing` dereferences to the buffer and the `Debug` redactions exist already
  (`Identity`, `BootstrapIdentity`, `Password`). Not chosen.
- **Include `opamp`.** It covers the copy that lives longest, but changes a clause of an accepted
  ADR in the publishable crate's interface, which supersedes that ADR. Left to its own decision.
- **Turn on `rcgen`'s `zeroize` feature.** Its `Zeroize` wipes only the serialised DER and runs
  only when called; the `ring` key stays. Not chosen.

## Sources / Prior art

- The locked sources under the Cargo registry, read 2026-10-20: `zeroize` 1.9.1; `rustls` 0.23.46
  `crypto/cipher.rs`, `crypto/hmac.rs`, `crypto/hpke.rs`, `crypto/ring/mod.rs`;
  `rustls-pki-types` 1.15.1 `lib.rs`; `ring` 0.17.14 `agreement.rs` and `ec/suite_b/ecdh.rs`;
  `rcgen` 0.14.10 `lib.rs`; `p12-keystore` 0.4.1; `pkcs12` 0.2.0-pre.0 `kdf.rs`; `pkcs8` 0.11.0
  `traits.rs`; `dialoguer` 0.12.0 `prompts/password.rs`. Read 2026-10-23: `tokio-postgres` 0.7.18
  `src/config.rs`; `object_store` 0.14.2 `src/aws/builder.rs` and `src/aws/credential.rs`.
- [`zeroize` 1.9.1](https://docs.rs/zeroize/1.9.1/zeroize/), its crate documentation read in the
  locked source: volatile writes and a compiler fence, and that a buffer reallocated or a value
  moved on the stack leaves copies it cannot reach.
- RustCrypto, `rustls` and `pkcs8` use `zeroize` for their own secrets, as the sources above show.

## Consequences

- Positive: a key, password or token this project's code dropped does not stay readable in freed
  memory. The dependency is one the tree already builds.
- Negative / trade-offs: the protection is partial.
  - `ring`, `reqwest` and `p12-keystore` keep copies that live as long as a connection or a parse.
  - `tokio-postgres` and `object_store` keep theirs as long as the Server's record and blob stores.
  - `opamp`'s `Identity` keeps the Client's key in plain memory.
  - A `Vec` that grows reallocates and leaves the old buffer behind, so a secret buffer is read at
    its final size.
- Negative / trade-offs: the signatures that carry a secret inside the application crates take and
  return `Zeroizing` types.
- Follow-ups: wiping the key of `opamp::tls::Identity`, which changes the dependency list of
  `opamp`'s features and so supersedes DEV-six-crates-tokio-axum; keeping secret pages out of swap
  and core dumps.

## Enforcement

Tests carrying `Verifies: SEC-secrets-wiped-on-drop` pin the types of the secrets the Applies-to
line names at compile time. Each binds a secret to a `Zeroizing` type, so a change back to a plain
buffer stops compiling (clauses 2 and 3):

- `crates/fleet-agent/src/tls.rs` `the_clients_secrets_are_held_in_zeroizing_buffers`: the key of
  `BootstrapIdentity`, `[packages] archive_key`, the key `key_file` reads, the password
  `read_password_file` reads, and the token a Supervisor Endpoint makes;
- `crates/fleet-core/src/pkcs12/file.rs` `the_key_a_file_opens_to_is_held_in_a_zeroizing_buffer`;
- `crates/fleet-server/src/bootstrap.rs` `a_bootstrap_password_is_held_in_a_zeroizing_buffer`;
- `crates/fleet-server/src/config.rs` `a_secret_file_is_read_into_a_zeroizing_buffer`;
- `crates/fleet-tools/src/pki.rs` `the_tools_hold_passwords_and_keys_in_zeroizing_buffers`.

Tests with the same marker hold that a secret buffer is read at its final size, so no outgrown copy
is left unwiped:

- `crates/fleet-agent/src/supervisor/channel.rs` `a_frame_never_outgrows_its_buffer`: a frame that
  carries a start's token is written into a buffer of its final size;
- `crates/fleet-agent/src/config_init.rs`
  `a_bootstrap_p12_or_a_password_file_over_its_bound_is_refused`: a bootstrap PKCS#12 file or a
  password file over its bound is refused before it is read;
- `crates/fleet-agent/src/config_file.rs`
  `a_privileged_load_refuses_a_configuration_over_the_bound`: a `supervisor.toml` over its bound is
  refused before it is read into a buffer that would grow.

That `zeroize` overwrites the bytes on drop is the crate's own tested behaviour. A test must not
read freed memory, since that is undefined behaviour.

**Not mechanically decidable:** whether a new secret is held in `Zeroizing` (clause 2), and whether
a copy is made earlier than clause 3 allows. No check can tell a secret from other bytes; review
decides it. Clauses 1 and 4 are held by review of the manifests and of the claims made about
libraries.
