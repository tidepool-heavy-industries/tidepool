#!/usr/bin/env bash
# Run the six declared helper/source contracts through their native counted owners.
# M1 production browser qualification separately uses the frozen deployment cohort.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
output="${1:?usage: run-main-acceptance.sh OUTPUT}"
[[ $# -eq 1 ]] || { echo 'error: expected a fresh evidence directory' >&2; exit 2; }
[[ ! -e "$output" ]] || { echo "error: evidence directory already exists: $output" >&2; exit 2; }
mkdir -m 700 -p "$output"
output="$(cd "$output" && pwd)"
run_native() {
  local label="$1" log="$output/${1##*:}.log"
  shift
  local args=(bash "$repo_root/scripts/buck2-run.sh" run --local-only -c remote.enabled=false "$label" -- "$@")
  printf '%q ' "${args[@]}" > "$log.command"
  printf '\n' >> "$log.command"
  if "${args[@]}" > "$log" 2>&1; then
    printf '0\n' > "$log.status"
    echo "passed: $label"
  else
    local status=$?
    printf '%s\n' "$status" > "$log.status"
    echo "FAILED: $label — $log" >&2
    return "$status"
  fi
}
# Rust owners each contain one real test; the prepared contract preserves eight
# runtime cases and source capture keeps its four host assertions diagnostic.
status=0
run_native //bridge/facade:facade_prepared_recipe_contract_test --expected-count 1 --jobs 1 \
  --output-dir "$output/prepared-runtime" || status=1
run_native //bridge/facade:facade_recipe_source_capture_test --expected-count 1 --jobs 1 \
  --output-dir "$output/source-capture" || status=1
# The shared Tasty owner discovers named leaves and rejects an empty selection.
for target in native_helper_contract pinned_source_contract automation_helper_contract browser_scenario_contract; do
  run_native "//bridge/haskell:$target" || status=1
done
exit "$status"
