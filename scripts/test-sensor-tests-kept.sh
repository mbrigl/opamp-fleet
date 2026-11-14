#!/usr/bin/env bash
#
# Self-test of scripts/sensor-tests-kept.sh.
#
# Each case builds a throwaway git repository under a temporary directory with a base commit
# holding two test files and one source file, applies one change as a second commit, runs the
# sensor against the pair, and asserts the exit code and the line the reviewer would read. The
# unchanged repository reports nothing; every other case is one thing the sensor reports, or one
# it must stay silent on, among them the timed waits a change adds to the Rust tests.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-tests-kept.sh scripts/lib/ .agents/skills/propose-adr/concepts/toolchain.md
#
# Usage:
#     scripts/test-sensor-tests-kept.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-tests-kept.sh"

# commit <dir> <message> — stage everything and commit with a fixed identity.
commit() {
  fixture "$1"
  git -C "${1:?}" add -A
  git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m "$2"
}

# repo <name> — a repository whose base commit is tagged 'base', printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/tests" "$dir/src"
  git -C "${dir:?}" init -q -b main
  printf 'def test_a():\n    assert True\n' > "$dir/tests/a_test.py"
  printf "it('b', () => {});\n" > "$dir/src/b.spec.ts"
  printf 'print("app")\n' > "$dir/src/app.py"
  commit "$dir" "base"
  git -C "${dir:?}" tag base
  echo "$dir"
}

# expect <exit code> <name> <dir> <needle> [base] — run the sensor in the repository against the
# given base (default 'base'); the output must contain the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" base="${5-base}"
  local out rc
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" "$base" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

d="$(repo unchanged)"
printf 'print("more")\n' >> "$d/src/app.py"
commit "$d" "a source change"
expect 0 "a change that touches no test reports nothing" "$d" "nothing to report"

d="$(repo test-deleted)"
git -C "${d:?}" rm -q tests/a_test.py
commit "$d" "drop a test"
expect 0 "a deleted test file" "$d" "deleted test file: tests/a_test.py"

d="$(repo test-moved-out)"
git -C "${d:?}" mv tests/a_test.py src/helper.py
commit "$d" "move a test out"
expect 0 "a test file renamed to a path that is no test" "$d" "moved out of the tests: tests/a_test.py -> src/helper.py"

d="$(repo test-moved-within)"
git -C "${d:?}" mv tests/a_test.py tests/a_more_test.py
commit "$d" "rename a test"
expect 0 "a test file renamed within the tests" "$d" "nothing to report"

d="$(repo source-deleted)"
git -C "${d:?}" rm -q src/app.py
commit "$d" "drop a source file"
expect 0 "a deleted source file is not a test" "$d" "nothing to report"

d="$(repo skip-added)"
printf "it.skip('c', () => {});\n" >> "$d/src/b.spec.ts"
commit "$d" "skip a test"
expect 0 "a skip marker added, with file and line" "$d" "skip or focus marker added: src/b.spec.ts:2: it.skip('c'"

d="$(repo focus-added)"
printf "describe.only('d', () => {});\n" >> "$d/src/b.spec.ts"
commit "$d" "focus a test"
expect 0 "a focus marker runs one test and skips the rest" "$d" "src/b.spec.ts:2: describe.only("

d="$(repo skip-in-source)"
printf '@unittest.skip("later")\n' >> "$d/src/app.py"
commit "$d" "skip somewhere else"
expect 0 "a marker in a file that is no test is reported too" "$d" "src/app.py:2: @unittest.skip"

d="$(repo skip-removed)"
printf "it.skip('c', () => {});\n" >> "$d/src/b.spec.ts"
commit "$d" "skip a test"
git -C "${d:?}" tag skipped
printf "it('b', () => {});\n" > "$d/src/b.spec.ts"
commit "$d" "unskip it"
expect 0 "a marker on a deleted line is not read" "$d" "nothing to report" skipped

d="$(repo toolchain-markers)"
printf 'func TestX(t *testing.T) {\n\tt.Skip("flaky")\n}\n' > "$d/tests/x_test.go"
printf '#[ignore]\nfn y() {}\n' > "$d/tests/y_test.rs"
printf '@Disabled\nvoid z() {}\n' > "$d/tests/ZTest.java"
commit "$d" "three toolchains"
expect 0 "the Go, Rust, and Java markers" "$d" "3 finding(s)"

# The catalogue's table names the markers a diff rule catches, for a reader choosing a toolchain;
# MARKERS of the sensor is what it matches, in the files of each toolchain. Every marker the table
# names is one the sensor finds in a file of its row's toolchain; a row this case knows no file of
# fails it, rather than leave its markers unchecked.
d="$(repo catalogue-markers)"
named=0
unknown=()
while IFS='|' read -r _ toolchain _ _ _ _ skipped _; do
  toolchain="${toolchain# }"
  # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
  mapfile -t markers < <(grep -o 'diff rule ([^)]*)' <<< "$skipped" | grep -oE '`[^`]+`' | tr -d '`')
  ((${#markers[@]})) || continue
  case "$toolchain" in
    *TypeScript*) file=tests/catalogue.test.ts ;;
    *Java*) file=tests/CatalogueTest.java ;;
    *Python*) file=tests/catalogue_test.py ;;
    Go*) file=tests/catalogue_test.go ;;
    *Rust*) file=tests/catalogue_test.rs ;;
    *C#*) file=tests/CatalogueTest.cs ;;
    *) unknown+=("$toolchain"); continue ;;
  esac
  printf '%s\n' "${markers[@]}" >> "$d/$file"
  named=$((named + ${#markers[@]}))
done < <(grep '^| ' "$ROOT/.agents/skills/propose-adr/concepts/toolchain.md" | grep -v '^| Toolchain |')
commit "$d" "every marker the catalogue names, each in a file of its toolchain"
if ((${#unknown[@]})); then
  failed=$((failed + 1)); echo "FAIL: catalogue rows with a diff rule and no file here:${unknown[*]}" >&2
else
  expect 0 "every marker the catalogue's table names is one the sensor finds" "$d" "$named finding(s)"
fi

# A marker is read only in the files of its toolchain: a Rust method named fit is no focused
# JavaScript test, nor is a method call on an object in TypeScript; the same names where they
# are markers are reported.
d="$(repo marker-in-another-language)"
printf 'fn g() {\n    let fits = self.content.fit(count);\n}\n' > "$d/src/g.rs"
printf 'const h = shape.fit(box);\n' > "$d/src/h.ts"
printf 'fit("x", () => {})\n' >> "$d/src/b.spec.ts"
printf '#[ignore]\nfn y() {}\n' > "$d/tests/y_test.rs"
printf '#[ignore]\n' > "$d/src/notes.md"
commit "$d" "fit in Rust and as a method, a focus and an ignore where they are markers"
expect 0 "a marker is read only in the files of its toolchain" "$d" "2 finding(s)"
expect 0 "a focus marker in TypeScript is reported" "$d" "src/b.spec.ts:2: fit("
expect 0 "an ignore in Rust is reported" "$d" "tests/y_test.rs:1: #[ignore]"

# The extension of a path git quotes or ends with a tab, a name with a space or a letter beyond
# ASCII, is read as the path's own.
d="$(repo marker-in-an-odd-path)"
printf '#[ignore]\nfn y() {}\n' > "$d/tests/a b_test.rs"
printf '#[ignore]\nfn z() {}\n' > "$d/tests/ü_test.rs"
commit "$d" "ignores in paths with a space and beyond ASCII"
expect 0 "a marker in a path with a space or beyond ASCII is reported" "$d" "2 finding(s)"

# Timed waits (docs/CONVENTIONS.md, "A test waits for its event, never for a time"): a sleep or a
# Duration literal added to a Rust test without a reason is listed; one with its reason, a wait
# through the waiter or under a timeout, and the same lines outside the tests, are not.
d="$(repo timed-sleep-in-a-test)"
mkdir -p "$d/crates/x/tests"
printf 'fn a() {\n    std::thread::sleep(std::time::Duration::from_millis(500));\n}\n' > "$d/crates/x/tests/a.rs"
commit "$d" "a test that sleeps"
expect 0 "a thread::sleep( added to a test without a reason is listed, with file and line" "$d" \
  "timed wait added: crates/x/tests/a.rs:2:     std::thread::sleep("
expect 0 "a sleep is one finding" "$d" "1 timed wait(s)"

d="$(repo timed-waits-with-their-reason)"
mkdir -p "$d/crates/x/tests"
cat > "$d/crates/x/tests/a.rs" <<'RS'
async fn a() {
    std::thread::sleep(Duration::from_millis(5)); // timed: the race this test provokes
    common::wait_until("the agent", || state.agent()).await;
    common::wait_until_within(Duration::from_secs(60), "the restart", || state.up()).await;
    tokio::time::timeout(Duration::from_secs(10), session.next()).await;
    assert_eq!(backoff.next(), Duration::from_secs(1));
}
RS
commit "$d" "waits with a reason, through the waiter, under a timeout"
expect 0 "a wait with its reason, through the waiter, under a timeout or in an assertion is not listed" "$d" \
  "nothing to report"

d="$(repo timed-every-form)"
mkdir -p "$d/crates/x/tests"
cat > "$d/crates/x/tests/a.rs" <<'RS'
async fn a() {
    tokio::time::sleep(step).await;
    tokio::time::sleep_until(then).await;
    let budget = Duration::new(3, 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    std::thread::sleep(step); // timed:
}
RS
commit "$d" "each form of a timed wait"
expect 0 "time::sleep(, sleep_until(, a Duration literal, and a reason left empty are each listed" "$d" \
  "5 timed wait(s)"

d="$(repo timed-in-cfg-test)"
mkdir -p "$d/crates/x/src"
cat > "$d/crates/x/src/lib.rs" <<'RS'
pub fn backoff() -> Duration {
    std::thread::sleep(Duration::from_millis(1));
    Duration::from_secs(1)
}

#[cfg(test)]
fn helper() {
    std::thread::sleep(Duration::from_millis(2));
}

pub fn after_the_helper() {
    std::thread::sleep(Duration::from_millis(3));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a() {
        let brace = "}";
        let close = '}';
        // }
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[cfg(not(test))]
fn production() {
    std::thread::sleep(Duration::from_millis(5));
}
RS
commit "$d" "sleeps in production code and in test code"
expect 0 "a sleep in a #[cfg(test)] item is listed, past braces in a string, a character or a comment" "$d" \
  "crates/x/src/lib.rs:22:         std::thread::sleep("
expect 0 "a sleep in a #[cfg(test)] function is listed" "$d" "crates/x/src/lib.rs:8:     std::thread::sleep("
expect 0 "production code before, between and after the #[cfg(test)] items is not read" "$d" "2 timed wait(s)"

d="$(repo timed-in-test-support)"
mkdir -p "$d/crates/x/src" "$d/tools"
printf 'pub fn settle() {\n    std::thread::sleep(Duration::from_millis(1));\n}\n' > "$d/crates/x/src/test_support.rs"
printf 'fn main() {\n    std::thread::sleep(Duration::from_millis(1));\n}\n' > "$d/tools/main.rs"
commit "$d" "a sleep in test support, and one in a tool"
expect 0 "a file of test support under src/ is test code; a tool outside crates/ is not read" "$d" \
  "1 timed wait(s) added to the tests"
expect 0 "the file of test support is the one listed" "$d" "crates/x/src/test_support.rs:2:"

d="$(repo timed-removed)"
mkdir -p "$d/crates/x/tests"
printf 'fn a() {\n    std::thread::sleep(Duration::from_millis(500));\n}\n' > "$d/crates/x/tests/a.rs"
commit "$d" "a test that sleeps"
git -C "${d:?}" tag slept
printf 'fn a() {\n    common::wait_until("it", || it()).await;\n}\n' > "$d/crates/x/tests/a.rs"
commit "$d" "a test that waits for its event"
expect 0 "a sleep replaced by a wait through the waiter is not listed" "$d" "nothing to report" slept

# The exemptions read the whole statement, as rustfmt breaks it over lines, and a reason on the
# comment line above; a statement that ended above the line exempts nothing.
d="$(repo timed-statements-over-lines)"
mkdir -p "$d/crates/x/tests"
cat > "$d/crates/x/tests/a.rs" <<'RS'
async fn a() {
    assert_eq!(
        backoff.at_least_a_step(Duration::from_secs(30)),
        Duration::from_secs(30)
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        session.next(),
    )
    .await;
    // timed: the race this test provokes
    std::thread::sleep(Duration::from_millis(5));
    let deadline = Instant::now() + Duration::from_secs(30);
}
RS
commit "$d" "statements over lines, and a reason above"
expect 0 "an assertion or a timeout over lines, and a reason above, exempt; an ended one does not" \
  "$d" "1 timed wait(s)"
expect 0 "the line after an ended statement is listed" "$d" "crates/x/tests/a.rs:13:"

# A test file moved is no line added: the sleeps it carries over are not listed, a new one is.
d="$(repo timed-moved)"
mkdir -p "$d/crates/x/tests"
printf 'fn a() {\n    std::thread::sleep(Duration::from_millis(500));\n    let x = 1;\n    let y = 2;\n}\n' \
  > "$d/crates/x/tests/a.rs"
commit "$d" "a test that sleeps"
git -C "${d:?}" tag sleeping
git -C "${d:?}" mv crates/x/tests/a.rs crates/x/tests/b.rs
printf 'fn c() {\n    std::thread::sleep(Duration::from_millis(1));\n}\n' >> "$d/crates/x/tests/b.rs"
commit "$d" "move it and add a sleep"
expect 0 "a moved test file's sleeps are not listed again" "$d" "1 timed wait(s)" sleeping
expect 0 "the sleep the move adds is" "$d" "crates/x/tests/b.rs:7:" sleeping

# The three ends of a #[cfg(test)] item: a ';' without braces, braces after a signature over lines
# and a where clause, and the attribute's other forms such as all(test, unix).
d="$(repo timed-cfg-test-forms)"
mkdir -p "$d/crates/x/src"
cat > "$d/crates/x/src/lib.rs" <<'RS'
#[cfg(test)]
mod tests;

pub fn first() {
    std::thread::sleep(Duration::from_millis(1));
}

#[cfg(test)]
fn helper<T>(
    a: u32,
    b: T,
) -> u32
where
    T: Clone,
{
    std::thread::sleep(Duration::from_millis(2));
    a
}

pub fn second() {
    std::thread::sleep(Duration::from_millis(3));
}

#[cfg(all(test, unix))]
fn on_unix() {
    std::thread::sleep(Duration::from_millis(4));
}

pub fn third() {
    std::thread::sleep(Duration::from_millis(5));
}
RS
commit "$d" "the forms of a cfg(test) item"
expect 0 "a braceless item ends at its ';', a signature over lines at its closing brace" "$d" \
  "2 timed wait(s)"
expect 0 "the item behind a signature and a where clause is read" "$d" "crates/x/src/lib.rs:16:"
expect 0 "an all(test, unix) item is read" "$d" "crates/x/src/lib.rs:26:"

# A #[cfg(test)] field of a function-pointer type ends at its ',': the 'fn' that opens an item is
# read at the start of the item only, and the production code after the field is not read.
d="$(repo timed-cfg-test-field)"
mkdir -p "$d/crates/x/src"
cat > "$d/crates/x/src/lib.rs" <<'RS'
struct S {
    #[cfg(test)]
    hook: fn(u8),
    other: u8,
}

pub fn prod() {
    std::thread::sleep(Duration::from_millis(1));
}
RS
commit "$d" "a cfg(test) field of a function-pointer type"
expect 0 "a #[cfg(test)] field of type fn(u8) ends at its ','" "$d" "nothing to report"

# A comment line within a statement does not end it.
d="$(repo timed-comment-in-a-statement)"
mkdir -p "$d/crates/x/tests"
cat > "$d/crates/x/tests/a.rs" <<'RS'
fn a() {
    assert_eq!(
        // the first step, whole
        backoff.next(),
        Duration::from_secs(1)
    );
}
RS
commit "$d" "an assertion with a comment line"
expect 0 "a comment line between assert_eq!( and its Duration keeps the exemption" "$d" \
  "nothing to report"

# A list of candidates beyond what one environment variable holds, 128 KiB, is read whole.
d="$(repo timed-many)"
mkdir -p "$d/crates/x/tests"
{
  echo 'fn a() {'
  for ((i = 0; i < 2500; i++)); do
    echo "    std::thread::sleep(std::time::Duration::from_millis($i)); // a line of some length"
  done
  echo '}'
} > "$d/crates/x/tests/a.rs"
commit "$d" "many sleeps"
expect 0 "2500 sleeps, a list larger than an environment variable holds, are each listed" "$d" \
  "2500 timed wait(s)"

# Called by its path from a subdirectory of another repository, the sensor reads the one it lives
# in, as the checks do: the tag 'base' exists only there.
# Verifies: ADR-0002
d="$(repo own-repository)"
mkdir -p "$d/scripts/lib" && cp "$SENSOR" "$d/scripts/" && cp "$ROOT/scripts/lib/git.sh" "$d/scripts/lib/"
git -C "${d:?}" rm -q tests/a_test.py
commit "$d" "drop a test"
other="$work/other"
mkdir -p "$other/sub" && git -C "$other" init -q -b main
out="$(cd "$other/sub" && bash "$d/scripts/sensor-tests-kept.sh" base 2>&1)"; rc=$?
if ((rc == 0)) && [[ "$out" == *"deleted test file: tests/a_test.py"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: a sensor called from another repository reads its own (exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
fi

d="$(repo same-commit)"
expect 0 "base and head the same commit" "$d" "base and head are the same commit"

d="$(repo bad-base)"
expect 2 "a base that does not resolve" "$d" "does not resolve" nosuchref

d="$(repo zero-base)"
git -C "${d:?}" rm -q tests/a_test.py
commit "$d" "drop a test"
expect 0 "a base of zeros counts as none and falls back to main, which is head" "$d" "nothing to compare" 0000000000000000000000000000000000000000

d="$(repo no-main)"
git -C "${d:?}" branch -m main work
expect 0 "no base and no main" "$d" "no main to compare with" ""

# --- summary ---------------------------------------------------------------------------------

summary "Tests kept sensor self-test"
