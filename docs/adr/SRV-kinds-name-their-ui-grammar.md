# SRV-kinds-name-their-ui-grammar: The bundled UI colours an Agent's effective configuration with Shiki, by the grammar the ADR of its kind names

- **Status:** 🟡 proposed
- **Date:** 2026-11-22
- **Deciders:** Markus Brigl
- **Applies to:** the effective-configuration cell and dialog of the Agents tab in `crates/fleet-server/static/index.html`, the vendored `crates/fleet-server/static/shiki.js` and the route that serves it in `crates/fleet-server/src/api.rs`, the token colours in `crates/fleet-server/static/ui.css`, `ui-carbon.css` and `ui-quiet.css`, the script that builds the vendored file under `scripts/`, `NOTICE`, and the grammar an ADR of a Supervisor kind names for its Agent type
- **Supersedes:** [SRV-ui-colours-with-shiki](SRV-ui-colours-with-shiki.md)

## Context

This ADR carries the five clauses of [SRV-ui-colours-with-shiki](SRV-ui-colours-with-shiki.md)
under their numbers and changes one: clause 3. There, the map from an Agent type to a grammar is a
fixed list. Every new Supervisor kind whose effective configuration is to be coloured then has to
supersede the whole UI decision to add one row, though nothing about Shiki, the bundle, its pin or
the colours changes. The `telegraf` kind was the first to meet this: its draft carried all of
SRV-ui-colours-with-shiki's clauses to change one line of clause 3.

Here clause 3 states a rule instead. The ADR of a kind names which of the bundled grammars colours
its Agent type's effective configuration, and the map in the UI follows those ADRs. The kind's
configuration language is the kind's own fact
([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)), so its ADR is where a
reader finds it. The Agent types that no kind ADR of their own decides keep the grammars
SRV-ui-colours-with-shiki gave them, named here.

SRV-ui-colours-with-shiki's own Context follows.

The Agents tab shows each Agent's effective configuration in a table cell and in a dialog. These
texts are long YAML or TOML files, hard to read without colour. The human wants them coloured with
TextMate grammars, the ones VS Code uses, and the colouring done locally in the browser. Only the
effective configuration is coloured; the Configuration editor is not.

Forces:

- The Server ships only a rudimentary UI for basic operation (Non-Goal *Shipping a production UI*
  of [`docs/SPECIFICATION.md`](../SPECIFICATION.md)). That UI is static assets embedded in the
  Server binary, plain HTML, CSS and JS, with **no frontend toolchain**, neither in the Dev
  Container nor in CI ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)). Redoc is
  vendored as one pre-built file and named in `NOTICE`; that is the precedent.
- The Operator plane's CSP is `script-src 'self'` plus the hashes of the inline scripts, and no
  `'wasm-unsafe-eval'`. TextMate grammars are written for the Oniguruma regex engine, which usually
  runs as WebAssembly. Allowing WebAssembly would weaken the policy (Q-1).
- Shiki is the library that brings VS Code's TextMate grammars to the browser. Its JavaScript regex
  engine (`@shikijs/engine-javascript`) translates the grammars' Oniguruma patterns into JavaScript
  regular expressions, with no WebAssembly. Shiki 4.5.0 is licensed under MIT.
- Shiki ships as ES modules with dependencies. It has no single pre-built browser file like
  Redoc's, so one has to be built once.
- The supply-chain check (Q-3, `cargo-deny`) covers Rust crates only. A vendored JavaScript file is
  outside it, as Redoc is.
- The Server knows no configuration format (Non-Goal *Replacing an agent's configuration
  language*). Choosing a grammar for display is presentation and changes nothing the Server
  stores, sends or decides.

## Decision

We will colour an Agent's effective configuration in the bundled UI with a pinned, vendored build
of Shiki, by the grammar its kind's ADR names.

1. **One vendored file, built once, never in CI.**
   - `crates/fleet-server/static/shiki.js` is a single browser script built from `shiki@4.5.0`
     with `@shikijs/engine-javascript`, the grammars of clause 3 and Shiki's CSS-variables theme.
     It exposes one function on `window`. The file starts with a header naming the packages,
     their versions and the script that built it.
   - `scripts/vendor-shiki.sh` rebuilds it. A developer runs it on their own machine, and only
     when the version is to change. CI and the Dev Container need no `node`; the bundled UI keeps
     no frontend toolchain ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)).
   - The Server embeds the file and serves it from its own origin as `/shiki.js`, like the
     stylesheets. The CSP does not change: `script-src 'self'` covers it, and nothing needs
     WebAssembly or `eval`.

2. **The version and the hash are pinned and checked.**
   - The file's SHA-256 and the version are written beside the build script.
   - A test fails when the embedded file's hash differs from the pinned one. The file therefore
     cannot change without the pin changing in the same commit.
   - `NOTICE` names Shiki and every package and grammar bundled, with their licences. Shiki and its
     packages are MIT. The YAML and TOML grammars come from TextMate's bundles under TextMate's own
     permissive notice, the INI grammar from VS Code under MIT.
   - The bundle's header carries both licence texts.

3. **The grammar follows the Agent type, as the ADR of its kind names it, and anything else stays
   plain.**
   - The UI chooses the grammar from the Agent's reported `service.name`.
   - The bundle holds three grammars: YAML, TOML and INI.
   - The ADR of a Supervisor kind must name which of the bundled grammars colours its Agent
     type's effective configuration, or that none does.
   - This ADR names the grammars of the Agent types that no kind ADR of their own decides:
     - `otelcol`, `otelcol-contrib` and any name starting `otelcol` → YAML;
     - this fleet's own Client (`supervisor`) → TOML;
     - `fluent-bit` → INI.
   - The map in `crates/fleet-server/static/index.html` holds exactly the grammars these ADRs
     name.
   - The UI shows the effective configuration of any other Agent type plain.
   - A grammar joins the bundle only when a kind's ADR needs one the bundle lacks. That ADR then
     changes the bundle under clauses 1 and 2.

4. **The colours are the look's.** The theme emits `var(--shiki-…)` colours. Each of `ui.css`,
   `ui-carbon.css` and `ui-quiet.css` defines them for its light and dark theme. A coloured
   configuration therefore follows the look and the theme the operator chose.
   - The theme is Shiki's CSS-variables theme with token rules of this project's on top. The
     CSS-variables theme alone leaves TOML table headers and keys, and everything in INI but its
     comments, in the foreground colour. The added rules map the TOML and INI scopes for table and
     section headers, keys, values and operators onto the same `--shiki-token-*` variables. No
     stylesheet needs a variable of its own for them.
   - An unquoted INI value, and a `key: value` or Fluent Bit `Name cpu` line, stay in the
     foreground colour. The bundled INI grammar gives them no scope of their own, and a rule on
     the whole `source.ini` would colour every line alike.

5. **Colouring is display only, and it never blocks.**
   - The UI colours the text after the table is drawn, on the cell and in the dialog.
   - If the script fails to load or a grammar throws, the plain text stays.
   - The UI escapes the text as for any other cell. Shiki builds its HTML from tokens of the text,
     not from markup in it.

**Out of scope:**

- colouring or editing the Configuration body;
- a code editor;
- a grammar per Configuration chosen by the operator;
- formatting (re-indenting) any configuration;
- WebAssembly in the browser.

## Alternatives considered

- **A fixed map in this ADR, as SRV-ui-colours-with-shiki has it.** It keeps every grammar in one
  place. Each kind that wants colour then supersedes the UI decision for one row, and the record
  carries the Shiki clauses again with every kind.
- **A grammar each kind states in code, which the Client reports.** It needs no map in the UI. The
  Server would carry a fact about a configuration language in the protocol, which the Non-Goal
  *Replacing an agent's configuration language* keeps out of it, and an Agent of no kind would
  report none.
- **highlight.js or Prism.** They are small (20–50 KB) and ship as single files. Their regular
  expressions are their own, not TextMate grammars, and colour YAML and TOML less exactly. The
  human asked for TextMate.
- **Shiki with the Oniguruma engine.** It is the reference engine, but it needs WebAssembly, so the
  CSP would need `'wasm-unsafe-eval'`. The JavaScript engine reaches the same grammars without it.
- **Loading Shiki from a CDN.** It needs no vendoring. It breaks the UI on a host without internet,
  hands the page to a third party, and would need the CSP opened to that origin.
- **A Node.js toolchain in CI that builds the file on every build.** It keeps the file reproducible
  at all times. [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) rejects this cost for a
  UI the specification keeps rudimentary (Non-Goal *Shipping a production UI*), and the pinned hash
  (clause 2) catches an unreviewed change.
- **Colouring on the Server.** Shiki could run there through a JavaScript runtime or a Rust port
  of TextMate (`syntect`). It adds a dependency to the Server binary for a display concern, and
  its colours would not follow the look.

## Sources / Prior art

- npm registry: `shiki` 4.5.0, released 2026-10-01, licence MIT. Its dependencies include
  `@shikijs/engine-javascript` and `@shikijs/langs`. The CSS-variables theme comes from Shiki's
  core, so `@shikijs/themes` is not bundled.
- Shiki documentation: the JavaScript regex engine and the CSS-variables theme.
- [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) on the bundled UI without a frontend
  toolchain, and its alternative "A frontend framework and bundler for the UI"; `NOTICE` (Redoc
  vendored as a pre-built file); the Operator plane's CSP in `crates/fleet-server/src/api.rs`.
- [SRV-ui-colours-with-shiki](SRV-ui-colours-with-shiki.md), whose clauses this ADR carries.

## Consequences

- Positive: effective configurations are readable, coloured with the grammars operators know from
  VS Code, in the look's own colours, with no change to the CSP and no network access.
- Positive: a new kind names its grammar in its own ADR and adds one entry to the map. No kind
  supersedes the UI decision for it.
- Negative / trade-offs: the grammars are named across several ADRs. A reader who wants the whole
  map reads it in `index.html`, which the test of clause 3 pins.
- Negative / trade-offs: the Server binary grows by the size of the bundle, about 190 KB.
- Negative / trade-offs: the JavaScript file is outside `cargo-deny`. The pinned hash makes a
  change visible but does not scan for vulnerabilities. Updating Shiki is a manual step with the
  script.
- Negative / trade-offs: the JavaScript engine can fail on a rare grammar pattern. The text then
  stays plain (clause 5).
- Follow-ups:
  - Colouring the Configuration body while it is edited.
  - A supply-chain check that covers vendored JavaScript as well as Rust crates.

## Enforcement

Tests that carry `Verifies: SRV-kinds-name-their-ui-grammar`, in
[`crates/fleet-server/tests/rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs):

- `the_ui_serves_the_pinned_shiki_bundle` holds clauses 1 and 2.
  - The embedded `shiki.js` has the pinned SHA-256.
  - `GET /shiki.js` answers the file with a JavaScript content type, under the unchanged CSP.
- `the_ui_colours_an_effective_configuration_by_the_agents_type` holds clauses 3 to 5.
  - The UI loads `/shiki.js`.
  - The map holds the grammars of clause 3 and the grammar each kind's ADR names.
  - Each of the three stylesheets defines the CSS-variables theme's colours for light and dark.

The two tests also carry `Verifies: SRV-ui-colours-with-shiki` until a human accepts this ADR.
That citation keeps the accepted ADR verified while it binds.

**Not mechanically decidable:** whether the ADR of a new kind names its grammar is read in review.
No test can read an ADR's prose for it.
