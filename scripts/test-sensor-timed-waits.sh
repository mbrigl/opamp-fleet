#!/usr/bin/env bash
#
# Self-test of scripts/sensor-timed-waits.sh.
#
# Verifies: NUC-checks-are-bash
#
# Each case builds a throwaway git repository under a temporary directory with a base commit,
# applies a change as a later commit, runs the sensor against the pair, and asserts the exit code
# and the line a reader would meet (docs/CONVENTIONS.md, "A test waits for its event, never for a
# time"): a sleep or a Duration literal added to a Rust test without a reason is listed; one with
# its reason, a wait through the waiter or under a timeout, and the same lines outside the tests,
# are not. The cases stood in scripts/test-sensor-tests-kept.sh while the section was part of
# that sensor. Last, base and head the same commit, and the two lines of scripts/check-all.sh that
# run the sensor.
#
# Inputs: scripts/sensor-timed-waits.sh scripts/lib/ scripts/check-all.sh
#
# Usage:
#     scripts/test-sensor-timed-waits.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-timed-waits.sh"

# commit <dir> <message> — stage everything and commit with a fixed identity.
commit() {
  fixture "$1"
  git -C "${1:?}" add -A
  git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m "$2"
}

# repo <name> — a repository whose base commit, a README, is tagged 'base', printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  printf '# Fixture\n' > "$dir/README.md"
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

d="$(repo same-commit)"
expect 0 "base and head the same commit" "$d" "base and head are the same commit"

check "check-all.sh runs the sensor" \
  grep -qx -- 'run "Timed waits sensor" scripts/sensor-timed-waits.sh' "$ROOT/scripts/check-all.sh"
check "check-all.sh runs the self-test" \
  grep -qx -- 'run "Timed waits sensor self-test" scripts/test-sensor-timed-waits.sh' \
  "$ROOT/scripts/check-all.sh"

# --- summary ---------------------------------------------------------------------------------

summary "Timed waits sensor self-test"
