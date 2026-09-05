#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -lt 1 || $# -gt 2 || ( $# -eq 2 && "$2" != "--list" ) ]]; then
  echo "usage: $0 <crate> [--list]" >&2
  exit 2
fi

crate="$1"
mode="${2:-run}"
scripts/test-suite-check.sh
mapfile -t targets < <(cargo metadata --no-deps --format-version 1 | jq -r --arg crate "$crate" \
  '.packages[] | select(.name == $crate) | .targets[] | select(.kind == ["test"]) | .name')
if [[ "${#targets[@]}" -eq 0 ]]; then
  if [[ "$mode" == "--list" ]]; then
    echo "$crate: whole-crate tests (no integration targets)"
    exit 0
  fi
  exec scripts/battery-shard.sh "$crate"
fi

total="${#targets[@]}"
if [[ "$mode" == "--list" ]]; then
  for index in "${!targets[@]}"; do
    echo "$((index + 1))/$total: ${targets[$index]}"
  done
  exit 0
fi

source scripts/lib-extract.sh
resolve_tidepool_extract
trap teardown_battery_daemon EXIT INT TERM
start_battery_daemon

for index in "${!targets[@]}"; do
  echo "==> suite $crate: shard $((index + 1))/$total"
  scripts/battery-shard.sh "$crate" --test "${targets[$index]}"
done
