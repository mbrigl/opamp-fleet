# Conventions — OpAMP Fleet

> How this project writes code and tests **where no check decides it**: the judgement calls. What
> a module may know, what a test asserts and never does, the shape of an error, how a thing is
> named when the glossary has no word for it. Whoever settles a convention updates this file
> inline, an agent included, in the same change as the review or code that settled it. The
> reviewer always has it ([NUC-documents-are-fixed §6](adr/NUC-documents-are-fixed.md)).

An entry says in a sentence or two what is done and why; the why tells a convention from a habit.
A convention a check can decide is the check, not an entry. One that constrains future choices is
an ADR. What the golden path already shows needs none. Few entries that hold beat many that are
skimmed. The design revision drops what prevented nothing
([`AGENTS.md` §5](../AGENTS.md#5-quality-bar--definition-of-done)).

- **A rule changes everywhere it is stated** — a change to a rule, a step, or a name searches
  the tree for its old wording and for every citation of its anchor, and corrects them in the
  same change. A link check sees the anchor, never a sentence that restates it.
- **A self-test asserts what it builds** — it takes its fixtures from the tree it copies or writes
  them itself, never from the template's own ADRs, scripts, or numbers, so it holds in a derived
  project and after a normalization.
- **A comment that relies on a decision names its clause** — `CLT-accounts-service-runs-under §25`,
  not the ADR alone, so that a normalization can re-cite it by its clause table. Several clauses
  take a section sign each, joined as the template's own text joins them: `§3, §5`, `§3 and §5`,
  `§3, §4, and §5`, a range `§3–§5`.
- **A test waits for its event, never for a time** — a test waits for what it needs to see, under a
  deadline only a defect reaches. A sleep or a timed budget that holds on an idle host fails on a
  loaded one, and a race the test meant to provoke goes unprovoked on a fast one. A wait that
  must be timed, such as the waiter's own poll, carries its reason as `// timed: <why>`, on its
  line or on a comment line right above it, which `scripts/sensor-timed-waits.sh` reads in place
  of listing it.
- **A structural test reads the syntax, never words** — a test that decides a rule over source or
  configuration parses it and asserts on items and keys. A word moves into a comment: the
  release-workflow test failed on one (a0dc7cd17), and a hand lexer missed test code that a syntax
  tree cuts (d54447ce8).
- **A test runs with the crate it decides** — so that crate's own `cargo test` runs it. The
  at-calls test sat in `fleet-util`, and the Client's tests never ran it (17e0754ef).
- **A rule two paths apply is one function** — two paths that admit the same input, or derive
  the same value from it, call one function. Two routes, a Gateway and the Server, or a list and
  a view of the same Agent are such paths. A second copy drifts: the path it misses admits what
  the other refuses, or shows what the other does not.
- **An item lives in the context it changes, and a test with the context it verifies** — in a
  module split by context, such as the Server's `fleet`, a method goes to the context whose state
  it changes, not to the one it reads, and one that two contexts would claim stays in the module
  root. A unit test goes to the context whose behaviour it decides, with the helpers only it
  uses. A reviewer reads from the file which context a change may affect, and a test elsewhere
  hides which context it pins.
- **A context calls another's private item through `pub(super)`, never `pub`** — a private
  method of one context that another context calls becomes `pub(super)`. A `pub` method in any
  context's `impl AppState` widens the public interface of `AppState` for every caller of the
  crate.
- **A context of `fleet` is not named after a module of its crate** — the contexts for Packages
  and enrolment are `fleet::delivery` and `fleet::approval`, not `packages` and `enrolment`.
  The crate has modules of those names, and a reader who sees `packages` in a path takes it for
  the store.
- **A caller tells errors apart by their kind, never by their text** — where a caller answers two
  errors differently, the error carries a kind it matches on. A message is reworded for the
  operator, and a match on its text then answers the wrong status without a test noticing.
- **A parser of `fleet-tools` that reads network bytes lives in a module that names nothing of
  the crate** — such as `fetch::upstream`, and its fuzz target compiles that file by `#[path]`.
  `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), and its parsers belong to no
  shared crate, so the path is the one way a fuzz target reaches them (Q-2).
