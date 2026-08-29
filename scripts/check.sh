#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-extract.sh
resolve_tidepool_extract
prepare_battery_artifacts check just check
cleanup_exit() {
  local status=$?
  finalize_battery_artifacts "$status"
  return "$status"
}
trap cleanup_exit EXIT
exec > >(tee -a "$BATTERY_NEXTEST_LOG") 2>&1

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --status-level fail --final-status-level fail
