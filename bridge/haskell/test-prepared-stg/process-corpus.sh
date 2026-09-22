#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
frontend="${TIDEPOOL_EXTRACT:?set TIDEPOOL_EXTRACT to the candidate frontend}"
worker="${TIDEPOOL_EXTRACT_WORKER:?set TIDEPOOL_EXTRACT_WORKER to the candidate worker}"
fixture="$repo_root/bridge/haskell/test/prepared-stg/PreparedStgProbe.hs"
resident_fixture="$repo_root/bridge/haskell/test/prepared-stg/ResidentPreparedProbe.hs"
resident_iface_writer="$repo_root/bridge/haskell/test/prepared-stg/MakeResidentIface.hs"
include="$repo_root/bridge/haskell/test/prepared-stg"
required_inventory="$repo_root/bridge/haskell/test-prepared-stg/required-inventory.fragments"
work="$(mktemp -d -t tidepool-m1-process.XXXXXX)"
daemon_pid=""

cleanup() {
  local status=$?
  if [[ -n "$daemon_pid" ]]; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ "$status" -eq 0 ]]; then
    rm -rf "$work"
  else
    echo "prepared process failure artifacts: $work" >&2
  fi
  return "$status"
}
trap cleanup EXIT INT TERM

run_direct() {
  local out="$1"
  mkdir -p "$out"
  TIDEPOOL_EXTRACT_WORKER="$worker" TIDEPOOL_PREPARED_STG_PROBE=1 \
    "$frontend" --include "$include" --target importedRetainedValueCall \
    --output-dir "$out" "$fixture" >"$out/stdout" 2>"$out/stderr"
}

run_resident_direct() {
  local out="$1"
  mkdir -p "$out"
  TIDEPOOL_EXTRACT_WORKER="$worker" TIDEPOOL_PREPARED_STG_PROBE=1 \
    "$frontend" --session-root "$work/session" \
    --inject-val Tidepool.Session.Val.G1 --target residentResult \
    --output-dir "$out" "$resident_fixture" >"$out/stdout" 2>"$out/stderr"
}

run_framed() {
  local out="$1"
  mkdir -p "$out"
  "$frontend" --connect "$work/extract.sock" --include "$include" \
    --target importedRetainedValueCall --output-dir "$out" "$fixture" \
    >"$out/stdout" 2>"$out/stderr"
}

run_resident_framed() {
  local out="$1"
  mkdir -p "$out"
  "$frontend" --connect "$work/extract.sock" \
    --session-root "$work/session" --inject-val Tidepool.Session.Val.G1 \
    --target residentResult --output-dir "$out" "$resident_fixture" \
    >"$out/stdout" 2>"$out/stderr"
}

expect_direct_failure() {
  local source="$1" label="$2" needle="$3"
  local out="$work/$label"
  mkdir -p "$out"
  if TIDEPOOL_EXTRACT_WORKER="$worker" TIDEPOOL_PREPARED_STG_PROBE=1 \
      "$frontend" --include "$include" --target malformedSite \
      --output-dir "$out" "$source" >"$out/stdout" 2>"$out/stderr"; then
    echo "$label unexpectedly succeeded" >&2
    return 1
  fi
  grep -q '"outcome":"source-failure"' "$out/stdout"
  grep -q "$needle" "$out/stdout"
}

expect_framed_failure() {
  local source="$1" label="$2" needle="$3"
  local out="$work/$label"
  mkdir -p "$out"
  if "$frontend" --connect "$work/extract.sock" --include "$include" \
      --target malformedSite --output-dir "$out" "$source" \
      >"$out/stdout" 2>"$out/stderr"; then
    echo "$label unexpectedly succeeded" >&2
    return 1
  fi
  grep -q '"outcome":"source-failure"' "$out/stdout"
  grep -q "$needle" "$out/stdout"
}

mkdir -p "$work/session"
runghc -package=ghc -i"$repo_root/bridge/haskell/src" "$resident_iface_writer" \
  "$(ghc --print-libdir)" "$work/session"

run_direct "$work/direct"
cat >"$work/InvalidPrepared.hs" <<'EOF'
module InvalidPrepared where
malformedSite =
EOF
expect_direct_failure "$work/InvalidPrepared.hs" direct-invalid 'parse error'
expect_direct_failure \
  "$include/MalformedPreparedSite.hs" direct-malformed 'is not fully applied'
run_direct "$work/direct-after-errors"
cmp "$work/direct/prepared-stg.inventory" \
  "$work/direct-after-errors/prepared-stg.inventory"
run_resident_direct "$work/resident-direct"

TIDEPOOL_EXTRACT_WORKER="$worker" TIDEPOOL_PREPARED_STG_PROBE=1 \
  "$frontend" --daemon --persistent --socket "$work/extract.sock" \
  >"$work/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 300); do
  [[ -S "$work/extract.sock" ]] && break
  kill -0 "$daemon_pid" 2>/dev/null || {
    cat "$work/daemon.log" >&2
    exit 1
  }
  sleep 0.1
done
[[ -S "$work/extract.sock" ]] || {
  echo "candidate daemon did not become ready" >&2
  exit 1
}

run_framed "$work/cold"
run_framed "$work/warm"
run_resident_framed "$work/resident-cold"
run_resident_framed "$work/resident-warm"
cmp "$work/direct/prepared-stg.inventory" "$work/cold/prepared-stg.inventory"
cmp "$work/direct/prepared-stg.inventory" "$work/warm/prepared-stg.inventory"
cmp "$work/resident-direct/prepared-stg.inventory" \
  "$work/resident-cold/prepared-stg.inventory"
cmp "$work/resident-direct/prepared-stg.inventory" \
  "$work/resident-warm/prepared-stg.inventory"

expect_framed_failure "$work/InvalidPrepared.hs" invalid 'parse error'
run_framed "$work/after-invalid"
cmp "$work/direct/prepared-stg.inventory" "$work/after-invalid/prepared-stg.inventory"
run_resident_framed "$work/resident-after-invalid"
cmp "$work/resident-direct/prepared-stg.inventory" \
  "$work/resident-after-invalid/prepared-stg.inventory"

expect_framed_failure \
  "$include/MalformedPreparedSite.hs" malformed 'is not fully applied'
run_framed "$work/after-malformed"
cmp "$work/direct/prepared-stg.inventory" "$work/after-malformed/prepared-stg.inventory"
run_resident_framed "$work/resident-after-malformed"
cmp "$work/resident-direct/prepared-stg.inventory" \
  "$work/resident-after-malformed/prepared-stg.inventory"

inventory="$work/direct/prepared-stg.inventory"
while IFS= read -r evidence; do
  [[ -z "$evidence" || "$evidence" == \#* ]] && continue
  grep -Fq "$evidence" "$inventory" || {
    echo "prepared inventory omitted: $evidence" >&2
    exit 1
  }
done <"$required_inventory"

resident_inventory="$work/resident-direct/prepared-stg.inventory"
for evidence in \
  'GlobalDependency(main:Tidepool.Session.Val.G1:retainedClosure)' \
  'GlobalDependency(main:Tidepool.Session.Val.G1:retainedEnvironment)' \
  'exactModule = "Tidepool.Session.Val.G1", exactOccurrence = "retainedClosure"' \
  'exactModule = "Tidepool.Session.Val.G1", exactOccurrence = "retainedEnvironment"'
do
  grep -Fq "$evidence" "$resident_inventory" || {
    echo "resident prepared inventory omitted: $evidence" >&2
    exit 1
  }
done

printf 'frontend_sha256=%s\n' "$(sha256sum "$frontend" | cut -d' ' -f1)"
printf 'worker_sha256=%s\n' "$(sha256sum "$worker" | cut -d' ' -f1)"
printf 'inventory_sha256=%s\n' "$(sha256sum "$inventory" | cut -d' ' -f1)"
printf 'resident_inventory_sha256=%s\n' \
  "$(sha256sum "$resident_inventory" | cut -d' ' -f1)"
echo "prepared process corpus passed: direct + framed cold/warm/invalid/recovery + injected resident imports"
