# SRV-ui-colours-with-shiki: The bundled UI colours an Agent's effective configuration with Shiki

- **Status:** 🟢 accepted
- **Date:** 2026-10-17
- **Deciders:** Markus Brigl
- **Applies to:** the effective-configuration cell and dialog of the Agents tab in `crates/fleet-server/static/index.html`, the vendored `crates/fleet-server/static/shiki.js` and the route that serves it in `crates/fleet-server/src/api.rs`, the token colours in `crates/fleet-server/static/ui.css`, `ui-carbon.css` and `ui-quiet.css`, the script that builds the vendored file under `scripts/`, and `NOTICE`

## Context

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

We will colour an Agent's effective configuration in the bundled UI with Shiki 4.5.0 and its
JavaScript regex engine, vendored as one pre-built file whose version and SHA-256 are pinned and
checked by a test, and whose colours come from each look's stylesheet.

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

3. **The grammar follows the Agent type, and anything else stays plain.** The UI chooses the
   grammar from the Agent's reported `service.name`:
   - `otelcol`, `otelcol-contrib` and any name starting `otelcol` → YAML;
   - this fleet's own Client (`supervisor`) → TOML;
   - `fluent-bit` → INI;
   - any other type → no colouring, the text shown plain.

   Only YAML, TOML and INI are bundled. A grammar is added when a supported kind needs it.

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

## Consequences

- Positive: effective configurations are readable, coloured with the grammars operators know from
  VS Code, in the look's own colours, with no change to the CSP and no network access.
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

Tests that carry `Verifies: SRV-ui-colours-with-shiki`, in
[`crates/fleet-server/tests/rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs):

- `the_ui_serves_the_pinned_shiki_bundle`: the embedded `shiki.js` has the pinned SHA-256, and
  `GET /shiki.js` answers the file with a JavaScript content type, under the unchanged CSP.
- `the_ui_colours_an_effective_configuration_by_the_agents_type`: the UI loads `/shiki.js` and
  maps `otelcol*` to YAML, `supervisor` to TOML, and `fluent-bit` to INI. Each of the three
  stylesheets defines the CSS-variables theme's colours for light and dark.
