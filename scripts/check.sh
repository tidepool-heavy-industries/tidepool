#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-extract.sh
source scripts/lib-steps.sh
resolve_tidepool_extract --prefer-persistent-daemon
prepare_battery_artifacts check just check
nextest_pid=""
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  teardown_battery_daemon
  return "$status"
}
trap cleanup_exit EXIT
on_signal() {
  [ -z "$nextest_pid" ] || _terminate_and_wait "$nextest_pid" "nextest"
  exit 130
}
trap on_signal INT TERM
exec > >(tee -a "$BATTERY_NEXTEST_LOG") 2>&1

run_default_tests() {
  # Mixed default-tier targets still compile Haskell. Reuse an outer daemon
  # when present; otherwise this check owns one for the entire test step.
  start_battery_daemon || return $?
  cargo nextest run --no-fail-fast --status-level fail --final-status-level fail &
  nextest_pid=$!
  local status=0
  wait "$nextest_pid" || status=$?
  nextest_pid=""
  return "$status"
}

# Lint and tests are independent: a lint failure must not hide test results.
run_step "scripts/lint.sh (fmt, clippy)" scripts/lint.sh
run_step "cargo nextest run (default tier)" run_default_tests

finish_steps "check"
