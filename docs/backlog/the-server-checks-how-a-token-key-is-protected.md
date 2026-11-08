# The Server checks how a key on the token is protected

**Kind:** hardening.

**Observed.** `Token::key` in `crates/fleet-server/src/pkcs11.rs` loads a CA key or the audit key
by its label and reads only its public half. It does not read `CKA_SENSITIVE`, `CKA_EXTRACTABLE`
or `CKA_NEVER_EXTRACTABLE`. A key an operator imported from the old key file, which the token
signing draft rejects for a move to the token, or one made extractable, is used without a word.
`opamp-fleetctl pki token-init` makes every key sensitive and never extractable, so the gap is a
key made by other tools.

**Idea.** At startup, refuse or warn about a key that is extractable, not sensitive, or was
extractable once (`CKA_NEVER_EXTRACTABLE` false), naming its label. Which of the two, refuse or
warn, is the human's.

**From.** The review of stage 3, ticket 12 (2026-11-08).
