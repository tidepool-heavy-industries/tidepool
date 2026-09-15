#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-extract.sh
source scripts/lib-steps.sh
resolve_tidepool_extract
prepare_battery_artifacts check just check
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  return "$status"
}
trap cleanup_exit EXIT
exec > >(tee -a "$BATTERY_NEXTEST_LOG") 2>&1

# Lint and tests are independent: a lint failure must not hide test results.
run_step "scripts/lint.sh (fmt, clippy)" scripts/lint.sh
run_step "cargo nextest run (default tier)" \
  cargo nextest run --no-fail-fast --status-level fail --final-status-level fail

finish_steps "check"
