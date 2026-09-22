#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# This is the one producer for artifacts that Rust embeds with include_bytes!.
# Each block below still invokes the owning Haskell/Rust producer; the script
# only provides one reproducible workspace-level entry point and copies the
# resulting artifact into its registered location.
source scripts/lib-extract.sh
resolve_tidepool_extract

work_root="$(mktemp -d "$repo_root/target/embedded-fixtures.XXXXXX")"
cleanup() {
  local status=$?
  if [[ "$status" -eq 0 ]]; then
    rm -rf -- "$work_root"
  else
    echo "embedded fixture generation failed; retained output: $work_root" >&2
  fi
  return "$status"
}
trap cleanup EXIT

fixture_root="$repo_root/bridge/haskell/test-prepared-stg/fixtures"
prepared_root="$repo_root/bridge/haskell/test-prepared-stg"
stdlib_root="$repo_root/bridge/haskell/lib"
frontend="$TIDEPOOL_EXTRACT"

mkdir -p "$work_root/m3" "$work_root/freer-resume" "$work_root/freer-retention"

# M3 is a projection contract, so its checked producer is the complete
# projection test rather than a direct target extraction.
( cd bridge/haskell && cabal test execution-schema-projection \
    --test-options="$work_root/m3/m3-vertical.cbor" \
    --test-show-details=direct )
cp -- "$work_root/m3/m3-vertical.cbor" "$fixture_root/m3-vertical.cbor"

"$frontend" --output-dir "$work_root/freer-resume" \
  --targets program,resumeInt,freerResumeEntries,askArgument,valResult \
  "$prepared_root/FreerResume.hs" \
  --include "$stdlib_root" --include "$prepared_root"
cp -- "$work_root/freer-resume/freerResumeEntries.prepared.cbor" \
  "$fixture_root/freer-resume.cbor"

"$frontend" --output-dir "$work_root/freer-retention" \
  --targets freerRequest "$prepared_root/FreerRetention.hs" \
  --include "$stdlib_root" --include "$prepared_root"
cp -- "$work_root/freer-retention/freerRequest.prepared.cbor" \
  "$fixture_root/freer-retention.cbor"

# The ignored integration test owns the multi-module retained-import setup and
# writes all three import artifacts atomically after both requests succeed.
cargo test --config 'build.rustc-wrapper=""' -p tidepool-extract-cmd \
  --test import_fixtures -- --ignored --nocapture

( cd bridge/haskell && cabal test execution-schema-encode \
    --test-option=--write-schema6-fixture \
    --test-option="$work_root/schema6-intrinsic.cbor" \
    --test-show-details=direct )
cp -- "$work_root/schema6-intrinsic.cbor" \
  "$repo_root/bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor"

python3 scripts/embedded-fixtures-check.py
