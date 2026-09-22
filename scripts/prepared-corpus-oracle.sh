#!/usr/bin/env bash
# Native GHC oracle for the Suite.hs prepared corpus.
#
# Values come from GHC evaluating Suite.hs natively, rendered by typed
# renderers (bridge/haskell/test-prepared-stg/suite-oracle). No expectation value is
# transcribed by hand. The only reviewed inputs are classifications
# (SuiteOracleClassifications.json) and permitted nontermination
# (SuiteOracleNonterminating.txt), and each carries a reason.
#
#   check  MANIFEST   compare the input fingerprint and the sealed payload digest
#                     (no GHC build)
#   verify MANIFEST   regenerate from native GHC and diff against the fixture
#   update MANIFEST   regenerate and write the fixture
#
# MANIFEST is the Suite projection manifest; its expectation keys are the
# oracle's domain.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

usage() {
  echo "usage: $0 check|verify|update MANIFEST" >&2
  exit 2
}
[[ $# -eq 2 ]] || usage
mode="$1"
manifest="$2"
case "$mode" in check | verify | update) ;; *) usage ;; esac

fixture="tidepool/prepared-corpus/fixtures/prepared-corpus-expectations.json"
oracle_source="bridge/haskell/test-prepared-stg/suite-oracle"
classifications="$oracle_source/SuiteOracleClassifications.json"
nonterminating="$oracle_source/SuiteOracleNonterminating.txt"
eval_timeout="${SUITE_ORACLE_TIMEOUT:-10s}"
required_ghc="9.12.2"
# The prepared pipeline's optimisation contract
# (Tidepool.GhcPipeline.canonicalizeDFlags). Nontermination shape depends on it.
ghc_flags=(-package ghc -O2 -fno-full-laziness -fno-cpr-anal
  -fexpose-all-unfoldings -fexpose-overloaded-unfoldings)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

jq -r '.programs[] | .expectation_key // empty' "$manifest" \
  | LC_ALL=C sort -u >"$work/names"

# Mirrors scripts/fixtures.sh: every input that can change a rendered value.
inputs_fingerprint() {
  {
    find bridge/haskell/lib "$oracle_source" -type f -print
    printf '%s\n' \
      flake.nix \
      flake.lock \
      bridge/haskell/cabal.project \
      bridge/haskell/test/Suite.hs \
      scripts/prepared-corpus-oracle.sh
  } | LC_ALL=C sort | xargs sha256sum
  printf 'flags %s\n' "${ghc_flags[*]}"
  printf 'ghc %s\n' "$required_ghc"
  printf 'timeout %s\n' "$eval_timeout"
  printf 'jq %s\n' "$(jq --version)"
  printf 'names %s\n' "$(sha256sum <"$work/names" | cut -d' ' -f1)"
} 2>/dev/null

fingerprint() { inputs_fingerprint | sha256sum | cut -d' ' -f1; }

# Digest of the complete checked-in oracle: values, refusals, source tops and
# classifications. `check` cannot regenerate values without GHC; the seal makes
# an edit after generation visible to it. `verify` remains the authority.
payload_digest() {
  jq -S -c 'del(.oracle_fingerprint, .oracle_payload_digest)' "$1" | sha256sum | cut -d' ' -f1
}

build_oracle() {
  local actual_ghc
  actual_ghc="$(ghc --numeric-version)"
  if [[ "$actual_ghc" != "$required_ghc" ]]; then
    echo "error: the Suite oracle requires GHC $required_ghc, found $actual_ghc" >&2
    exit 1
  fi
  if ! SUITE_ORACLE_NAMES="$work/names" ghc --make "${ghc_flags[@]}" \
    -i"$repo_root/bridge/haskell/lib" -i"$repo_root/bridge/haskell/test" -i"$repo_root/$oracle_source" \
    -outputdir "$work/build" -o "$work/suite-oracle" \
    "$repo_root/$oracle_source/SuiteOracle.hs" >"$work/ghc.log" 2>&1; then
    cat "$work/ghc.log" >&2
    echo "error: the Suite oracle did not build" >&2
    exit 1
  fi
}

generate() {
  local output="$1"
  build_oracle
  "$work/suite-oracle" domain >"$work/domain.jsonl"

  if ! jq -e -n --slurpfile domain "$work/domain.jsonl" --slurpfile classes "$classifications" '
    ($domain | map({key: .occurrence, value: .}) | from_entries) as $d
    | $classes[0] | to_entries
    | all(.[]; $d[.key] != null and $d[.key].class != "value"
        and (.value.reason | type == "string" and length > 0)
        and (.value.expectation.kind == "cyclic_observation"))
  ' >/dev/null; then
    echo "error: each classification must name a manifest key without a native value" \
      "oracle, give a reason, and declare cyclic_observation: $classifications" >&2
    exit 1
  fi

  : >"$work/values.jsonl"
  local occurrence json status reason
  while IFS= read -r occurrence; do
    set +e
    json="$(timeout "$eval_timeout" "$work/suite-oracle" eval "$occurrence" 2>"$work/eval.err")"
    status=$?
    set -e
    case "$status" in
      0)
        jq -cn --arg key "$occurrence" --argjson value "$json" \
          '{key: $key, value: $value}' >>"$work/values.jsonl"
        ;;
      3 | 4)
        jq -cn --arg key "$occurrence" --rawfile reason "$work/eval.err" \
          '{key: $key, refusal: {class: "unrepresentable", reason: ($reason | rtrimstr("\n"))}}' \
          >>"$work/values.jsonl"
        ;;
      124)
        reason="$(awk -F '\t' -v o="$occurrence" '$1 == o { print $2 }' "$nonterminating")"
        if [[ -z "$reason" ]]; then
          echo "error: $occurrence did not terminate within $eval_timeout and is not" \
            "listed in $nonterminating" >&2
          exit 1
        fi
        jq -cn --arg key "$occurrence" '{key: $key, value: {kind: "no_finite_observation"}}' \
          >>"$work/values.jsonl"
        ;;
      *)
        echo "error: native evaluation of $occurrence exited $status: $(cat "$work/eval.err")" >&2
        exit 1
        ;;
    esac
  done < <(jq -r 'select(.scope == "source_top" and .class == "value") | .occurrence' \
    "$work/domain.jsonl")

  while IFS=$'\t' read -r occurrence _; do
    [[ -z "$occurrence" || "$occurrence" == \#* ]] && continue
    if ! jq -e --arg key "$occurrence" \
      'select(.key == $key and .value.kind == "no_finite_observation")' \
      "$work/values.jsonl" >/dev/null; then
      echo "error: $nonterminating lists $occurrence, but it terminated or is not a" \
        "closed source top" >&2
      exit 1
    fi
  done <"$nonterminating"

  jq -n -S \
    --arg fingerprint "$(fingerprint)" \
    --arg ghc "$required_ghc" --arg flags "${ghc_flags[*]}" \
    --slurpfile domain "$work/domain.jsonl" \
    --slurpfile values "$work/values.jsonl" \
    --slurpfile classes "$classifications" '
    {
      source_revision: ("generated by scripts/prepared-corpus-oracle.sh from bridge/haskell/test/Suite.hs: native GHC "
        + $ghc + " " + $flags),
      oracle_fingerprint: $fingerprint,
      source_tops: ([$domain[] | select(.scope == "source_top") | .occurrence] | sort),
      refusals: (
        [$domain[] | select(.scope == "source_top" and .class != "value")
          | {key: .occurrence, value: {class, reason}}]
        + [$values[] | select(.refusal) | {key, value: .refusal}]
        | from_entries),
      expectations: (
        ([$values[] | select(.value) | {key, value}] | from_entries)
        + ($classes[0] | map_values(.expectation)))
    }' >"$work/unsealed.json"
  jq -S --arg digest "$(payload_digest "$work/unsealed.json")" \
    '. + {oracle_payload_digest: $digest}' "$work/unsealed.json" >"$output"
}

case "$mode" in
  check)
    expected="$(fingerprint)"
    actual="$(jq -r '.oracle_fingerprint // empty' "$fixture")"
    if [[ "$actual" != "$expected" ]]; then
      echo "error: the Suite oracle is stale; run: scripts/prepared-corpus-oracle.sh update $manifest" >&2
      exit 1
    fi
    sealed="$(jq -r '.oracle_payload_digest // empty' "$fixture")"
    if [[ "$sealed" != "$(payload_digest "$fixture")" ]]; then
      echo "error: $fixture changed after generation; regenerate it:" \
        "scripts/prepared-corpus-oracle.sh update $manifest" >&2
      exit 1
    fi
    echo "Suite oracle fingerprint and payload seal are current"
    ;;
  verify)
    generate "$work/expectations.json"
    if ! diff -u <(jq -S . "$fixture") "$work/expectations.json"; then
      echo "error: the regenerated Suite oracle differs from $fixture" >&2
      exit 1
    fi
    echo "Suite oracle regenerates identically"
    ;;
  update)
    generate "$work/expectations.json"
    cp -- "$work/expectations.json" "$fixture"
    echo "updated $fixture"
    ;;
esac
