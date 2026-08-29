#!/usr/bin/env bash
# Run ONE GHC-heavy crate's tests, as a survivable shard of the full battery.
#
# `scripts/battery.sh` runs the ENTIRE workspace (`--ignore-default-filter`)
# in one process, which is hours-long here and gets hard-killed by this
# environment's ~380s background-process cap long before it finishes.
# Splitting by crate makes full coverage achievable as a sequence of shards,
# each of which fits under that cap (modulo the TIDEPOOL_EXPENSIVE_TESTS=1
# suites — see below).
#
# Usage: scripts/battery-shard.sh <crate> [extra nextest args...]
#   scripts/battery-shard.sh tidepool-runtime
#   scripts/battery-shard.sh tidepool-codegen -E 'binary(proptest_ghc_idioms)'
#
# This does NOT set TIDEPOOL_EXPENSIVE_TESTS — the expensive suites
# (corpus_report, haskell_suite_differential, tidepool-testing::haskell_verified)
# stay skipped unless you export TIDEPOOL_EXPENSIVE_TESTS=1 yourself. Run
# those deliberately, one at a time, with their own budget — they are NOT
# what this script's ~380s-per-shard promise covers; haskell_verified in
# particular runs for multiple hundreds of seconds.
# `corpus_report` and `haskell_suite_differential` are ALSO `#[ignore]`d, so
# reaching them additionally needs `--run-ignored all` scoped with `-E` (a
# bare `--run-ignored all` also un-ignores tidepool-codegen's deliberately-off
# known-bug repros and heavy fuzz lanes — see scripts/battery.sh), e.g.:
#   TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh tidepool-codegen \
#     --run-ignored all -E 'test(haskell_suite_differential) or test(corpus_report)'
#
# Large crates are partitioned by the checked `dev/test-suites.json` manifest.
# Run every partition sequentially, with one shared compile daemon, via
# `just suite <crate>`.
#
# Resident compile daemon: same
# per-run daemon scripts/battery.sh can start — see its header for the full
# rationale. ON BY DEFAULT (kill switch: TIDEPOOL_EXTRACT_NO_DAEMON=1). This script
# starts one (or reuses an outer wrapper's, e.g. when chained across the
# sub-shard groups above) via the shared lib-extract.sh helpers, exports the
# socket for the nextest invocation below, and tears it down (by exact pid,
# escalating to SIGKILL after a 10s grace period) on exit, including
# SIGINT/SIGTERM. TIDEPOOL_EXTRACT_NO_DAEMON=1 is the kill switch.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ $# -lt 1 ]; then
  echo "usage: $0 <crate> [extra nextest args...]" >&2
  exit 1
fi

crate="$1"
shift

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

source "$(dirname "${BASH_SOURCE[0]}")/lib-extract.sh"
resolve_tidepool_extract

# Per-run resident compile daemon; see scripts/battery.sh for the rationale.
# This mirrors it via the shared lib-extract.sh helpers rather than
# duplicating the logic. On by default (kill switch: TIDEPOOL_EXTRACT_NO_DAEMON=1).
# Outer-wrapper respect: when battery-shard.sh runs as one leg of a chain
# (scripts/battery-shard.sh's own header documents the multi-shard sequence
# for tidepool-harness/runtime/repl), a daemon already started by an earlier
# leg or an enclosing script is reused, not restarted or torn down here.
nextest_pid=""
prepare_battery_artifacts "battery-$crate" scripts/battery-shard.sh "$crate" "$@"
tmp_log="$BATTERY_NEXTEST_LOG"
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  teardown_battery_daemon
  return "$status"
}
trap cleanup_exit EXIT
# See scripts/battery.sh's on_signal comment: a signal sent directly to this
# script's pid isn't delivered to a synchronous foreground command, so
# nextest runs backgrounded + waited (below) to make it interruptible.
on_signal() {
  echo "==> signal received — stopping nextest and tearing down the compile daemon" >&2
  # See scripts/battery.sh's on_signal comment: waits for nextest to
  # actually exit before returning, so no child compile remains alive.
  [ -n "$nextest_pid" ] && _terminate_and_wait "$nextest_pid" "nextest"
  exit 130
}
trap on_signal INT TERM
start_battery_daemon

echo "==> shard: -p ${crate} (--ignore-default-filter, TIDEPOOL_EXPENSIVE_TESTS=${TIDEPOOL_EXPENSIVE_TESTS:-unset})"

# `exec` here would replace this shell before any check could run — so a run
# that SELECTS ZERO TESTS (a typo'd -E filter, a crate/filter combination with
# nothing left after --ignore-default-filter) would inherit nextest's exit
# code and nothing else, and read as a pass whenever that code is 0. Capture
# the run instead: report a genuine test failure AND an empty selection if
# both occur (never let one mask the other), and propagate the real exit
# status. stderr is where nextest writes everything (PASS/FAIL/Summary lines
# included; stdout is otherwise unused) — tee it through unchanged via a
# process substitution so a caller piping/redirecting stderr still sees the
# identical live stream.
set +e
cargo nextest run --ignore-default-filter --no-fail-fast \
  --status-level fail --final-status-level fail \
  -p "$crate" "$@" 2> >(tee "$tmp_log" >&2) &
nextest_pid=$!
wait "$nextest_pid"
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: shard selected/ran ZERO tests for -p ${crate} — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
