#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-extract.sh
prepare_battery_artifacts quick just quick
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  return "$status"
}
trap cleanup_exit EXIT
exec > >(tee -a "$BATTERY_NEXTEST_LOG") 2>&1

mapfile -t packages < <(python3 scripts/test-changed.py --quick-packages)
args=()
for package in "${packages[@]}"; do args+=(-p "$package"); done
cargo nextest run --profile battery --lib "${args[@]}" --status-level fail --final-status-level fail
