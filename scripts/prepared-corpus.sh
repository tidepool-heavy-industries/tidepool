#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

output_root="$repo_root/target/prepared-corpus"
mkdir -p "$output_root"
priority_root="$(mktemp -d "$output_root/project-work-candidate.XXXXXX")"
actor_root="$(mktemp -d "$output_root/agent-watch-await-settled.XXXXXX")"
suite_root="$(mktemp -d "$output_root/suite.XXXXXX")"

echo "==> building prepared corpus runner"
cargo build -p tidepool-testing --bin prepared-corpus
prepared_runner="$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.target_directory')/debug/prepared-corpus"

echo "==> building prepared corpus projection probe"
( cd haskell && cabal build execution-corpus-projection )
projection_probe="$(cd haskell && cabal list-bin execution-corpus-projection)"

metadata="$repo_root/haskell/test/suite_cbor/meta.cbor"
priority_expectations="$(mktemp)"
suite_targets="$(mktemp)"
priority_targets="$(mktemp)"
actor_targets="$(mktemp)"
trap 'rm -f "$priority_expectations" "$suite_targets" "$priority_targets" "$actor_targets"' EXIT

# Project.Work is a priority production-facing source, but it has no expected
# value oracle. Keep its expectations explicitly empty so the Rust runner can
# record missing comparison evidence rather than treating it as a pass.
printf '%s\n' '{"source_revision":"none","expectations":{}}' >"$priority_expectations"
printf '%s\n' candidate >"$priority_targets"

priority_source="$priority_root/source/Project.Work.hs"
priority_include="$priority_root/source"
mkdir -p "$priority_include/Project"
ln -s "$repo_root/tidepool/src/actor_host/fixtures/project/Work.hs" \
  "$priority_source"
ln -s "$repo_root/tidepool/src/actor_host/fixtures/project/Types.hs" \
  "$priority_include/Project/Types.hs"
echo "==> projecting priority Project.Work.candidate"
"$projection_probe" \
  "$priority_source" Project.Work "$priority_targets" "$priority_root" \
  "$repo_root/haskell/lib" "$priority_include"
priority_report="$priority_root/results.json"
"$prepared_runner" run \
  "$priority_root/manifest.json" "$priority_expectations" "$metadata" \
  "$priority_report"

printf '%s\n' awaitSettled >"$actor_targets"
echo "==> projecting actor stdlib Tidepool.Agent.Watch.awaitSettled"
"$projection_probe" \
  "$repo_root/haskell/lib/Tidepool/Agent/Watch.hs" Tidepool.Agent.Watch \
  "$actor_targets" "$actor_root" "$repo_root/haskell/lib"
actor_report="$actor_root/results.json"
"$prepared_runner" run \
  "$actor_root/manifest.json" "$priority_expectations" "$metadata" \
  "$actor_report"

find "$repo_root/haskell/test/suite_cbor" -maxdepth 1 -type f -name '*.cbor' \
  ! -name meta.cbor -printf '%f\n' \
  | sed 's/\.cbor$//' \
  | LC_ALL=C sort >"$suite_targets"
suite_count="$(wc -l <"$suite_targets" | tr -d '[:space:]')"
echo "==> projecting Suite.hs prepared corpus ($suite_count targets)"
"$projection_probe" \
  --all-tops "$repo_root/haskell/test/Suite.hs" Suite "$suite_targets" "$suite_root" \
  "$repo_root/haskell/lib"
suite_report="$suite_root/results.json"
"$prepared_runner" run \
  "$suite_root/manifest.json" \
  "$repo_root/tidepool-testing/fixtures/prepared-corpus-expectations.json" \
  "$metadata" "$suite_report"

report_totals() {
  local cohort="$1"
  local report="$2"
  echo "  $cohort stage totals:"
  jq -r '.stage_totals[] |
    "    \(.stage): passed=\(.passed) failed=\(.failed) missing_expectation=\(.missing_expectation) not_reached=\(.not_reached)"' \
    "$report"
}

echo "prepared corpus results:"
echo "  priority: $priority_root"
echo "  actor stdlib: $actor_root"
echo "  suite:    $suite_root"
echo "  suite targets recorded: $suite_count"
echo "  comparison expectations are historical and may be missing; inspect result rows"
report_totals priority "$priority_report"
report_totals actor-stdlib "$actor_report"
report_totals suite "$suite_report"

report_legacy_totals() {
  local cohort="$1"
  local manifest="$2"
  jq -r --arg cohort "$cohort" '
    ([.legacy_targets[] | select(.identity != null)] | length) as $mapped
    | ([.legacy_targets[] | select(.identity == null)] | length) as $unmapped
    | (.programs | length) as $stg_tops
    | "  \($cohort) manifest: stg_tops=\($stg_tops) legacy_mapped=\($mapped) legacy_unmapped=\($unmapped)"
  ' "$manifest"
}

report_legacy_totals priority "$priority_root/manifest.json"
report_legacy_totals actor-stdlib "$actor_root/manifest.json"
report_legacy_totals suite "$suite_root/manifest.json"
