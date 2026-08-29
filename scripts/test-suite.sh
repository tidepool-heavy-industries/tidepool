#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -lt 1 || $# -gt 2 || ( $# -eq 2 && "$2" != "--list" ) ]]; then
  echo "usage: $0 <crate> [--list]" >&2
  exit 2
fi

crate="$1"
mode="${2:-run}"
manifest="dev/test-suites.json"

if ! jq -e --arg crate "$crate" 'has($crate)' "$manifest" >/dev/null; then
  if [[ "$mode" == "--list" ]]; then
    echo "$crate: one whole-crate shard (no partition manifest)"
    exit 0
  fi
  echo "==> no multi-shard manifest for $crate; running it as one crate shard"
  exec scripts/battery-shard.sh "$crate"
fi

scripts/test-suite-check.sh

mapfile -t groups < <(jq -c --arg crate "$crate" '.[$crate][]' "$manifest")
total="${#groups[@]}"
if [[ "$mode" == "--list" ]]; then
  for index in "${!groups[@]}"; do
    names="$(jq -r 'join(", ")' <<<"${groups[$index]}")"
    echo "$((index + 1))/$total: $names"
  done
  exit 0
fi

source scripts/lib-extract.sh
resolve_tidepool_extract
trap teardown_battery_daemon EXIT INT TERM
start_battery_daemon

for index in "${!groups[@]}"; do
  filter="$(jq -r 'map("binary(" + . + ")") | join(" or ")' <<<"${groups[$index]}")"
  echo "==> suite $crate: shard $((index + 1))/$total"
  scripts/battery-shard.sh "$crate" -E "$filter"
done
