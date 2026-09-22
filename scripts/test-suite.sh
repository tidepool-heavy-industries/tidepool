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
metadata="$(cargo metadata --no-deps --format-version 1)"
target_list="$(jq -er --arg crate "$crate" '
  . as $metadata | [.packages[] | select(.name == $crate) |
    select(.id as $id | $metadata.workspace_members | index($id))] |
  if length != 1 then error("expected one workspace package: " + $crate)
  else [.[0].targets[] | select(.kind == ["test"]) | .name] | join("\n") end
' <<<"$metadata")"
targets=()
if [[ -n "$target_list" ]]; then mapfile -t targets <<<"$target_list"; fi
if [[ "${#targets[@]}" -eq 0 ]]; then
  if [[ "$mode" == "--list" ]]; then
    echo "$crate: whole-crate tests (no integration targets)"
    exit 0
  fi
  exec scripts/battery.sh -p "$crate"
fi

total="${#targets[@]}"
if [[ "$mode" == "--list" ]]; then
  for index in "${!targets[@]}"; do
    echo "$((index + 1))/$total: ${targets[$index]}"
  done
  exit 0
fi

# One invocation builds all selected targets and schedules their tests under
# the same concurrency limits. The battery owns signals, cleanup, and failure
# reporting, including failures in more than one integration target.
target_args=()
for target in "${targets[@]}"; do target_args+=(--test "$target"); done
exec scripts/battery.sh -p "$crate" "${target_args[@]}"
