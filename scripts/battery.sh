#!/usr/bin/env bash
# Run selected tests through nextest, with one process per test and a shared
# extractor daemon. Prefer explicit Cargo targets to avoid compiling unused
# integration suites:
#   scripts/battery.sh -p tidepool-runtime --test session -E 'test(user_library::)'
# Without package/target arguments this builds and runs the whole workspace.
# Expensive and known-bug ignored tests remain opt-in: use narrowly scoped
# --run-ignored all and TIDEPOOL_EXPENSIVE_TESTS=1 when required by the test.
#
# Resident compile daemon: this
# script can start (or, if $TIDEPOOL_EXTRACT_DAEMON_SOCKET is already set and
# live, reuse) a per-run tidepool-extract compile daemon and export the
# socket for the whole nextest invocation below, amortizing the ~5s
# GHC-boot-plus-stdlib-typecheck tax every extract spawn otherwise pays.
# It is on by default; TIDEPOOL_EXTRACT_NO_DAEMON=1 is the kill switch. The daemon is
# always torn down (by its exact recorded pid, escalating to SIGKILL after a
# 10s grace period if it doesn't exit on TERM) on script exit, including
# SIGINT/SIGTERM — see lib-extract.sh's
# start_battery_daemon/teardown_battery_daemon. Failure to connect needs no
# handling here: ExtractCmd::run() safely falls back before submission. A
# failure after submission is surfaced rather than replayed and duplicating
# an in-flight GHC compile (tidepool-extract-cmd/CLAUDE.md).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

source "$(dirname "${BASH_SOURCE[0]}")/lib-extract.sh"
resolve_tidepool_extract

# Per-run resident compile daemon amortizes the GHC startup and stdlib
# typecheck cost across every extraction in this run.
# On by default (kill switch: TIDEPOOL_EXTRACT_NO_DAEMON=1). Outer-wrapper respect:
# reuses an already-live $TIDEPOOL_EXTRACT_DAEMON_SOCKET instead of starting
# a second one (see lib-extract.sh's start_battery_daemon doc). The trap is
# installed BEFORE start_battery_daemon runs so a signal mid-boot still
# tears the daemon down; nextest_pid starts empty since on_signal may fire
# before it's set. (start_battery_daemon is always called — it's a no-op
# when the daemon is disabled.)
nextest_pid=""
prepare_battery_artifacts battery scripts/battery.sh "$@"
tmp_log="$BATTERY_NEXTEST_LOG"
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  teardown_battery_daemon
  return "$status"
}
trap cleanup_exit EXIT
# A signal sent directly to this script's pid (as opposed to a terminal
# Ctrl-C, which hits the whole foreground process group including nextest)
# is not delivered to a synchronous foreground command — bash defers trap
# execution until that command finishes on its own. Running nextest in the
# background and blocking on `wait` (below) makes the signal interrupt that
# wait immediately, so this fires promptly either way.
on_signal() {
  echo "==> signal received — stopping nextest and tearing down the compile daemon" >&2
  # Waits for nextest to actually exit (escalating to KILL after a grace
  # period if needed) before this function returns — never exit while
  # nextest or its own child tidepool-extract compiles might still be
  # alive, so cleanup remains exact and bounded.
  [ -n "$nextest_pid" ] && _terminate_and_wait "$nextest_pid" "nextest"
  exit 130
}
trap on_signal INT TERM
start_battery_daemon

# --ignore-default-filter: the full battery runs EVERY crate, including the
# GHC-extract-heavy ones that .config/nextest.toml's default-filter skips for
# quick inner-loop `cargo nextest run`. Same profile, so slow-timeout + the
# ghc-heavy thread cap still apply.
#
# `exec` here would replace this shell before any check could run — same
# zero-tests-as-a-pass trap scripts/battery-shard.sh closes; see that script's
# comment for the reasoning. Capture the run instead of masking it behind exec.
set +e
# No hardcoded --workspace: the root manifest is VIRTUAL, so a bare
# invocation already defaults to every member (script cd's to repo root
# above) — while an explicit `--workspace` OVERRIDES any caller-passed
# `-p <crate>`, silently building and listing the whole workspace when
# the caller asked for one crate (found live: `-p tidepool-mcp` ran 3612
# tests, not 175). Passing "$@" bare lets `-p` actually scope.
#
# Backgrounded + waited (rather than run directly in the foreground) so a
# signal sent to this script's own pid interrupts promptly — see on_signal
# above.
cargo nextest run --ignore-default-filter --no-fail-fast \
  --status-level fail --final-status-level fail \
  "$@" 2> >(tee "$tmp_log" >&2) &
nextest_pid=$!
wait "$nextest_pid"
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: battery selected/ran ZERO tests — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
