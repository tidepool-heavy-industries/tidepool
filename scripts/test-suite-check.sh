#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
manifest="dev/test-suites.json"

jq empty "$manifest"
status=0
scratch="$(mktemp -d -t tidepool-suite-check.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT

while IFS= read -r crate; do
  declared="$scratch/$crate.declared"
  actual="$scratch/$crate.actual"

  jq -r --arg crate "$crate" '.[$crate][][]' "$manifest" | sort >"$declared"

  duplicates="$(uniq -d "$declared")"
  if [[ -n "$duplicates" ]]; then
    echo "error: duplicate binaries in $crate suite:" >&2
    printf '  %s\n' "${duplicates//$'\n'/$'\n  '}" >&2
    status=1
  fi

  {
    find "$crate/tests" -maxdepth 1 -type f -name '*.rs' -printf '%f\n' \
      | sed 's/\.rs$//'
    find "$crate/tests" -mindepth 2 -maxdepth 2 -type f -name main.rs -printf '%h\n' \
      | sed 's#.*/##'
  } | sort -u >"$actual"

  missing="$(comm -23 "$actual" "$declared")"
  stale="$(comm -13 "$actual" "$declared")"
  if [[ -n "$missing" ]]; then
    echo "error: $crate integration binaries missing from dev/test-suites.json:" >&2
    printf '  %s\n' "${missing//$'\n'/$'\n  '}" >&2
    status=1
  fi
  if [[ -n "$stale" ]]; then
    echo "error: $crate suite names without a matching integration binary:" >&2
    printf '  %s\n' "${stale//$'\n'/$'\n  '}" >&2
    status=1
  fi
done < <(jq -r 'keys[]' "$manifest")

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi

echo "test suite manifest covers every declared crate binary exactly once"
