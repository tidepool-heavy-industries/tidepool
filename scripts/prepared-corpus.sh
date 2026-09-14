#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

output_root="$repo_root/target/prepared-corpus"
mkdir -p "$output_root"
run_root="$(mktemp -d "$output_root/run.XXXXXX")"
priority_root="$(mktemp -d "$output_root/project-work-candidate.XXXXXX")"
actor_root="$(mktemp -d "$output_root/agent-watch-await-settled.XXXXXX")"
suite_root="$(mktemp -d "$output_root/suite.XXXXXX")"
recovered_root="$(mktemp -d "$output_root/recovered-base-contract.XXXXXX")"
formatting_root="$(mktemp -d "$output_root/formatting-execution-contract.XXXXXX")"
formatting_shadow_root="$(mktemp -d "$output_root/formatting-dependency-shadow.XXXXXX")"
fingerprint_root="$(mktemp -d "$output_root/fingerprint-execution-contract.XXXXXX")"
containers_root="$(mktemp -d "$output_root/containers-contract.XXXXXX")"
bignum_root="$(mktemp -d "$output_root/bignum-contract.XXXXXX")"
usertypes_root="$(mktemp -d "$output_root/usertypes-contract.XXXXXX")"
text_root="$(mktemp -d "$output_root/text-contract.XXXXXX")"
echo "==> prepared corpus executable snapshot: $run_root"
echo "    provenance: $run_root/provenance.json"

echo "==> building prepared corpus runner"
cargo build -p tidepool-testing --bin prepared-corpus
built_runner="$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.target_directory')/debug/prepared-corpus"
prepared_runner="$run_root/prepared-corpus"
cp --reflink=auto -- "$built_runner" "$prepared_runner"

echo "==> building prepared corpus projection probe"
( cd haskell && cabal build execution-corpus-projection )
built_probe="$(cd haskell && cabal list-bin execution-corpus-projection)"
projection_probe="$run_root/execution-corpus-projection"
cp --reflink=auto -- "$built_probe" "$projection_probe"
chmod a-w -- "$prepared_runner" "$projection_probe"

runner_hash="$(sha256sum -- "$prepared_runner" | cut -d ' ' -f1)"
probe_hash="$(sha256sum -- "$projection_probe" | cut -d ' ' -f1)"
git_head="$(git rev-parse HEAD)"
git_dirty=false
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  git_dirty=true
fi
jq -n \
  --arg git_head "$git_head" --argjson git_dirty "$git_dirty" \
  --arg runner_source "$built_runner" --arg runner_path "$prepared_runner" \
  --arg runner_sha256 "$runner_hash" \
  --arg probe_source "$built_probe" --arg probe_path "$projection_probe" \
  --arg probe_sha256 "$probe_hash" \
  '{git_head: $git_head, git_dirty: $git_dirty,
    runner: {source: $runner_source, path: $runner_path, sha256: $runner_sha256},
    projection_probe: {source: $probe_source, path: $probe_path, sha256: $probe_sha256}}' \
  >"$run_root/provenance.json"
effects_core="$("$prepared_runner" effects-core)"

metadata="$repo_root/haskell/test/suite_cbor/meta.cbor"
suite_targets="$(mktemp)"
trap 'rm -f "$suite_targets"' EXIT

assert_contract_report() {
  local cohort="$1"
  local expected="$2"
  local report="$3"
  jq -e --argjson expected "$expected" '
    .stg_programs == $expected
    and (.programs | length) == $expected
    and ([.stage_totals[].stage] | sort) ==
      (["projection", "validation", "admission", "compilation", "execution", "comparison"] | sort)
    and all(.stage_totals[];
      .passed == $expected and .failed == 0 and .missing_expectation == 0
      and .running == 0 and .not_reached == 0)
    and all(.programs[];
      (.stages | length) == 6 and all(.stages[]; .outcome.status == "passed"))
  ' "$report" >/dev/null || {
    echo "contract cohort $cohort did not pass all six stages for $expected rows: $report" >&2
    return 1
  }
}

priority_source="$repo_root/haskell/test-prepared-stg/ProjectWorkCandidate.hs"
priority_include="$priority_root/source"
mkdir -p "$priority_include/Project"
ln -s "$repo_root/tidepool/src/actor_host/fixtures/project/Work.hs" \
  "$priority_include/Project/Work.hs"
ln -s "$repo_root/tidepool/src/actor_host/fixtures/project/Types.hs" \
  "$priority_include/Project/Types.hs"
echo "==> projecting priority Project.Work.candidate structural probe"
"$projection_probe" \
  "$priority_source" ProjectWorkCandidate \
  "$repo_root/haskell/test-prepared-stg/ProjectWorkCandidateTargets" "$priority_root" \
  "$repo_root/haskell/lib" "$priority_include"
priority_report="$priority_root/results.json"
"$prepared_runner" run \
  "$priority_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/ProjectWorkCandidateExpectations.json" "$metadata" \
  "$priority_report"
assert_contract_report priority-project-work 1 "$priority_report"

echo "==> projecting actor awaitSettled dependency probe"
actor_source="$repo_root/haskell/test-prepared-stg/AwaitSettledDependencies.hs"
"$projection_probe" \
  "$actor_source" AwaitSettledDependencies \
  "$repo_root/haskell/test-prepared-stg/AwaitSettledDependenciesTargets" \
  "$actor_root" "$repo_root/haskell/lib" "$effects_core"
actor_report="$actor_root/results.json"
"$prepared_runner" run \
  "$actor_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/AwaitSettledDependenciesExpectations.json" "$metadata" \
  "$actor_report"
assert_contract_report actor-await-settled-dependencies 1 "$actor_report"

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

echo "==> projecting recovered base-call contract (1 target)"
"$projection_probe" \
  "$repo_root/haskell/test-prepared-stg/RecoveredBody.hs" RecoveredBody \
  "$repo_root/haskell/test-prepared-stg/RecoveredBodyTargets" "$recovered_root" \
  "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
recovered_report="$recovered_root/results.json"
"$prepared_runner" run \
  "$recovered_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/RecoveredBodyExpectations.json" \
  "$metadata" "$recovered_report"
assert_contract_report recovered-base 1 "$recovered_report"

echo "==> projecting formatting execution contract (5 targets)"
"$projection_probe" \
  "$repo_root/haskell/test-prepared-stg/FormattingExecutionContract.hs" FormattingExecutionContract \
  "$repo_root/haskell/test-prepared-stg/FormattingExecutionTargets" "$formatting_root" \
  "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
formatting_report="$formatting_root/results.json"
"$prepared_runner" run \
  "$formatting_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/FormattingExecutionExpectations.json" \
  "$metadata" "$formatting_report"
assert_contract_report formatting-execution 5 "$formatting_report"

echo "==> projecting formatting dependency-shadow contract (1 target)"
"$projection_probe" \
  "$repo_root/haskell/test-prepared-stg/FormattingDependencyShadow.hs" FormattingDependencyShadow \
  "$repo_root/haskell/test-prepared-stg/FormattingDependencyShadowTargets" "$formatting_shadow_root" \
  "$repo_root/haskell/test-prepared-stg/formatting-dependency-shadow" \
  "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
formatting_shadow_report="$formatting_shadow_root/results.json"
"$prepared_runner" run \
  "$formatting_shadow_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/FormattingDependencyShadowExpectations.json" \
  "$metadata" "$formatting_shadow_report"
assert_contract_report formatting-dependency-shadow 1 "$formatting_shadow_report"

echo "==> projecting fingerprint execution contract (3 targets)"
"$projection_probe" \
  "$repo_root/haskell/test-prepared-stg/FingerprintExecutionContract.hs" FingerprintExecutionContract \
  "$repo_root/haskell/test-prepared-stg/FingerprintExecutionTargets" "$fingerprint_root" \
  "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
fingerprint_report="$fingerprint_root/results.json"
"$prepared_runner" run \
  "$fingerprint_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/FingerprintExecutionExpectations.json" \
  "$metadata" "$fingerprint_report"
assert_contract_report fingerprint-execution 3 "$fingerprint_report"

# Pure-evaluation cohorts. Each probe is a nullary monomorphic top whose value
# comes from an oracle compiled by the pinned GHC, so a stage failure here is an
# engine boundary rather than a disputed expectation.
run_pure_cohort() {
  local cohort="$1"
  local module="$2"
  local root="$3"
  local expected="$4"
  echo "==> projecting $cohort pure-eval cohort ($expected targets)"
  "$projection_probe" \
    "$repo_root/haskell/test-prepared-stg/${module}.hs" "$module" \
    "$repo_root/haskell/test-prepared-stg/${module}Targets" "$root" \
    "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
  "$prepared_runner" run \
    "$root/manifest.json" \
    "$repo_root/haskell/test-prepared-stg/${module}Expectations.json" \
    "$metadata" "$root/results.json"
  assert_contract_report "$cohort" "$expected" "$root/results.json"
}

run_pure_cohort containers ContainersContract "$containers_root" 8
run_pure_cohort bignum BignumContract "$bignum_root" 6
run_pure_cohort usertypes UserTypesContract "$usertypes_root" 8
run_pure_cohort text TextContract "$text_root" 8

report_totals() {
  local cohort="$1"
  local report="$2"
  echo "  $cohort stage totals:"
  jq -r '.stage_totals[] |
    "    \(.stage): passed=\(.passed) failed=\(.failed) missing_expectation=\(.missing_expectation) not_reached=\(.not_reached)"' \
    "$report"
}

echo "prepared corpus results:"
echo "  executables and provenance: $run_root"
echo "  priority: $priority_root"
echo "  actor stdlib: $actor_root"
echo "  suite:    $suite_root"
echo "  suite targets recorded: $suite_count"
echo "  recovered base contract: $recovered_root"
echo "  formatting execution contract: $formatting_root"
echo "  formatting dependency-shadow contract: $formatting_shadow_root"
echo "  fingerprint execution contract: $fingerprint_root"
echo "  containers cohort: $containers_root"
echo "  bignum cohort: $bignum_root"
echo "  usertypes cohort: $usertypes_root"
echo "  text cohort: $text_root"
echo "  comparison expectations are historical and may be missing; inspect result rows"
echo "  named limitation: awaitSettled's continuation is not executed by this dependency-only probe"
report_totals priority "$priority_report"
report_totals actor-stdlib "$actor_report"
report_totals suite "$suite_report"
echo "  separate acceptance contracts (not part of Suite or legacy counts):"
report_totals recovered-base "$recovered_report"
report_totals formatting-execution "$formatting_report"
report_totals formatting-dependency-shadow "$formatting_shadow_report"
report_totals fingerprint-execution "$fingerprint_report"
echo "  pure-evaluation cohorts (oracle-backed, pinned GHC 9.12.2):"
report_totals containers "$containers_root/results.json"
report_totals bignum "$bignum_root/results.json"
report_totals usertypes "$usertypes_root/results.json"
report_totals text "$text_root/results.json"

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
