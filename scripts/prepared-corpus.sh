#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
metadata_mode="${1:-check}"
if [[ $# -gt 1 || ( "$metadata_mode" != check && "$metadata_mode" != update ) ]]; then
  echo "usage: $0 [check|update]" >&2
  exit 2
fi

output_root="$repo_root/target/prepared-corpus"
mkdir -p "$output_root"
work_root="$(mktemp -d "$output_root/run.XXXXXX")"
run_root="$work_root/executables"
priority_root="$work_root/project-work-candidate"
actor_root="$work_root/agent-watch-await-settled"
suite_root="$work_root/suite"
recovered_root="$work_root/recovered-base-contract"
formatting_root="$work_root/formatting-execution-contract"
formatting_shadow_root="$work_root/formatting-dependency-shadow"
fingerprint_root="$work_root/fingerprint-execution-contract"
containers_root="$work_root/containers-contract"
bignum_root="$work_root/bignum-contract"
usertypes_root="$work_root/usertypes-contract"
text_root="$work_root/text-contract"
time_root="$work_root/time-intrinsic-contract"
mkdir -p "$run_root" "$priority_root" "$actor_root" "$suite_root" "$recovered_root" "$formatting_root" "$formatting_shadow_root" "$fingerprint_root" "$containers_root" "$bignum_root" "$usertypes_root" "$text_root" "$time_root"
suite_targets="$work_root/suite-targets"
cleanup_corpus() {
  local status=$?
  if [[ "$status" -eq 0 ]]; then
    # Retain bounded summary evidence; binaries and per-top scratch are
    # regenerable. Failure runs keep the exact executables and full reports.
    jq '{cohort: (input_filename | split("/")[-2]), stage_totals, stg_programs}' \
      "$work_root"/*/results.json | jq -s --slurpfile provenance "$run_root/provenance.json" \
      '{provenance: $provenance[0], cohorts: .}' >"$work_root/summary.json"
    mv "$work_root/summary.json" "$output_root/latest-success.json"
    rm -rf -- "$work_root"
    echo "==> corpus passed; summary: $output_root/latest-success.json"
  else
    echo "==> corpus failed; retained evidence: $work_root" >&2
  fi
  return "$status"
}
trap cleanup_corpus EXIT
echo "==> prepared corpus executable snapshot: $run_root"
echo "    provenance: $run_root/provenance.json"

echo "==> building prepared corpus runner"
cargo build -p tidepool-prepared-corpus --bin prepared-corpus
cargo build -p tidepool-testing --bin test-effects-core
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
effects_generator="$(dirname "$built_runner")/test-effects-core"
effects_core="$("$prepared_runner" effects-core "$("$effects_generator")")"

metadata="$repo_root/haskell/test/suite_cbor/meta.cbor"

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
      and .not_closed == 0 and .no_finite_observation == 0 and .function_valued == 0
      and .no_oracle == 0 and .running == 0 and .not_reached == 0)
    and all(.programs[];
      (.stages | length) == 6 and all(.stages[]; .outcome.status == "passed"))
  ' "$report" >/dev/null || {
    echo "contract cohort $cohort did not pass all six stages for $expected rows: $report" >&2
    return 1
  }
}

# Suite.hs includes compiler-introduced workers, dictionaries, and floats. Their
# number and whether GHC makes them independently callable are implementation
# details, so this contract derives the source domain from the native oracle
# rather than pinning a total-row count or aggregate pass floor. It still
# requires every emitted row to pass projection through compilation, and then
# checks each declared source top against its exact oracle or typed refusal.
assert_suite_report() {
  local cohort="$1"
  local report="$2"
  local oracle="$3"
  local manifest="$4"
  local expected_source_targets="$5"
  jq -e \
    --argjson expected_source_targets "$expected_source_targets" \
    --slurpfile oracle "$oracle" \
    --slurpfile manifest "$manifest" '
    $oracle[0] as $oracle |
    $manifest[0] as $manifest |
    ($manifest.programs | map({key: .name, value: .expectation_key}) | from_entries) as $keys |
    def expectation_key: $keys[.name];
    def is_source:
      expectation_key as $key | ($oracle.source_tops | index($key)) != null;
    def status($index): .stages[$index].outcome.status;
    def source_kind:
      expectation_key as $key | $oracle.expectations[$key].kind;
    def refusal_class:
      expectation_key as $key | $oracle.refusals[$key].class;
    (.source_targets | length) == $expected_source_targets
    and .source_mapped == $expected_source_targets
    and .source_unmapped == 0
    and ([.source_targets[].source_name] | sort) == ($oracle.source_tops | sort)
    and (.stg_programs == (.programs | length))
    and ([.programs[].name] | sort) == ([$manifest.programs[].name] | sort)
    # Every compiler-emitted program remains an acceptance obligation even
    # though the compiler may harmlessly add, remove, or reshape such rows.
    and all(.programs[];
      (.stages | length) == 6
      and all(.stages[0:4][]; .outcome.status == "passed")
      and all(.stages[]; .outcome.status != "running")
      and status(4) != "failed"
      and status(5) != "failed")
    # Values and error oracles must compare exactly. The two source-level
    # refusal classes and no-finite oracle have their own typed outcomes.
    and all(.programs[] | select(is_source);
      if refusal_class == "not_closed" then
        status(4) == "classified" and .stages[4].outcome.class == "not_closed"
          and status(5) == "not_reached"
      elif refusal_class == "unrepresentable" then
        status(4) == "passed" and status(5) == "missing_expectation"
      elif source_kind == "no_finite_observation" or source_kind == "cyclic_observation" then
        status(4) == "classified" and .stages[4].outcome.class == "no_finite_observation"
          and status(5) == "not_reached"
      else
        status(5) == "passed"
      end)
    # A missing expectation is permitted only for the reviewed source-level
    # unrepresentable values; compiler rows must remain explicitly no-oracle.
    and all(.programs[] | select(status(5) == "missing_expectation");
      is_source and refusal_class == "unrepresentable")
    and all(.programs[];
      if .stages[4].outcome.status == "passed"
      then .stages[5].outcome.status != "not_reached" else true end)
  ' "$report" >/dev/null || {
    echo "suite cohort $cohort regressed: source domain or source oracle mismatch," \
      "or an emitted row failed projection through execution: $report" >&2
    return 1
  }
}

jq -r '.source_tops[]' "$repo_root/tidepool-prepared-corpus/fixtures/prepared-corpus-expectations.json" \
  | LC_ALL=C sort >"$suite_targets"
suite_count="$(wc -l <"$suite_targets" | tr -d '[:space:]')"
echo "==> projecting Suite.hs prepared corpus ($suite_count targets)"
"$projection_probe" \
  --metadata-targets 'con_left con_right con_just con_nothing showInt' \
  --all-tops "$repo_root/haskell/test/Suite.hs" Suite "$suite_targets" "$suite_root" \
  "$repo_root/haskell/lib"
if [[ "$metadata_mode" == update ]]; then
  cp "$suite_root/meta.cbor" "$metadata"
elif ! cmp -s "$suite_root/meta.cbor" "$metadata"; then
  echo "error: $metadata is stale; run just fixtures-update" >&2
  exit 1
fi
suite_report="$suite_root/results.json"
suite_oracle="$repo_root/tidepool-prepared-corpus/fixtures/prepared-corpus-expectations.json"
echo "==> checking the generated Suite oracle against this manifest"
"$repo_root/scripts/prepared-corpus-oracle.sh" check "$suite_root/manifest.json"
jq -e '.source_tops | type == "array"' "$suite_oracle" >/dev/null || {
  echo "Suite oracle must declare its source_tops domain: $suite_oracle" >&2
  exit 1
}
"$prepared_runner" run \
  "$suite_root/manifest.json" "$suite_oracle" \
  "$metadata" "$suite_report"
assert_suite_report suite "$suite_report" "$suite_oracle" "$suite_root/manifest.json" "$suite_count"
jq -r '[.stage_totals[] | select(.stage == "execution")][0]
  | "  Suite execution: \(.passed) passed, \(.failed) failed;"
    + " classified: not_closed=\(.not_closed) no_finite_observation=\(.no_finite_observation)"
    + " function_valued=\(.function_valued)"' "$suite_report"

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

echo "==> projecting time intrinsic contract (4 targets)"
"$projection_probe" \
  "$repo_root/haskell/test-prepared-stg/TimeIntrinsicContract.hs" TimeIntrinsicContract \
  "$repo_root/haskell/test-prepared-stg/TimeIntrinsicTargets" "$time_root" \
  "$repo_root/haskell/lib" "$repo_root/haskell/test-prepared-stg"
time_report="$time_root/results.json"
"$prepared_runner" run \
  "$time_root/manifest.json" \
  "$repo_root/haskell/test-prepared-stg/TimeIntrinsicExpectations.json" \
  "$metadata" "$time_report"
assert_contract_report time-intrinsic 4 "$time_report"

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

echo "==> checking pure-eval cohort probes against the committed opacity manifest"
"$repo_root/scripts/probe-opacity-check.sh"

report_totals() {
  local cohort="$1"
  local report="$2"
  echo "  $cohort stage totals:"
  jq -r '.stage_totals[] |
    "    \(.stage): passed=\(.passed) failed=\(.failed) not_closed=\(.not_closed) no_finite_observation=\(.no_finite_observation) function_valued=\(.function_valued) missing_expectation=\(.missing_expectation) no_oracle=\(.no_oracle) not_reached=\(.not_reached)"' \
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
echo "  time intrinsic contract: $time_root"
echo "  containers cohort: $containers_root"
echo "  bignum cohort: $bignum_root"
echo "  usertypes cohort: $usertypes_root"
echo "  text cohort: $text_root"
echo "  comparison expectations are historical and may be missing; inspect result rows"
echo "  named limitation: awaitSettled's continuation is not executed by this dependency-only probe"
report_totals priority "$priority_report"
report_totals actor-stdlib "$actor_report"
report_totals suite "$suite_report"
echo "  separate acceptance contracts (not part of Suite or source counts):"
report_totals recovered-base "$recovered_report"
report_totals formatting-execution "$formatting_report"
report_totals formatting-dependency-shadow "$formatting_shadow_report"
report_totals fingerprint-execution "$fingerprint_report"
echo "  pure-evaluation cohorts (oracle-backed, pinned GHC 9.12.2):"
report_totals containers "$containers_root/results.json"
report_totals bignum "$bignum_root/results.json"
report_totals usertypes "$usertypes_root/results.json"
report_totals text "$text_root/results.json"

report_source_totals() {
  local cohort="$1"
  local manifest="$2"
  jq -r --arg cohort "$cohort" '
    ([.source_targets[] | select(.identity != null)] | length) as $mapped
    | ([.source_targets[] | select(.identity == null)] | length) as $unmapped
    | (.programs | length) as $stg_tops
    | "  \($cohort) manifest: stg_tops=\($stg_tops) source_mapped=\($mapped) source_unmapped=\($unmapped)"
  ' "$manifest"
}

report_source_totals priority "$priority_root/manifest.json"
report_source_totals actor-stdlib "$actor_root/manifest.json"
report_source_totals suite "$suite_root/manifest.json"
