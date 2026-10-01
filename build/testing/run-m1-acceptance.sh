#!/usr/bin/env bash
# Run inside the admitted pinned Nix shell after the matched producer is frozen.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

: "${IN_NIX_SHELL:?run this harness through scripts/dev-shell.sh}"
: "${M1_EVIDENCE:?set a fresh ignored evidence directory}"
: "${M1_FRONTEND:?set the admitted matched frontend path}"
: "${M1_WORKER:?set the admitted matched worker path}"
: "${M1_FRONTEND_SHA256:?set its recorded digest}"
: "${M1_WORKER_SHA256:?set its recorded digest}"
: "${M1_PRODUCER_EVIDENCE:?set the owning producer provenance JSON path}"
: "${M1_PRODUCER_EVIDENCE_SHA256:?set the explicitly admitted producer packet digest}"
: "${M1_BROWSER_PACKAGE_LOCK:?set the matching browser package-lock.json}"
: "${TIDEPOOL_BROWSER_NODE:?set the pinned Nix Node executable}"
: "${PLAYWRIGHT_BROWSERS_PATH:?set the pinned Nix browser closure}"
: "${TIDEPOOL_BROWSER_DRIVER:?set the reviewed production browser driver path}"
: "${M1_BROWSER_NODE_MODULES:?set the installed matching browser dependency directory}"
: "${EXOMONAD_EMBEDDED_ASSET_ROOT:?set the reviewed production GUI assets}"

if [ -e "$M1_EVIDENCE" ]; then
  echo 'M1_EVIDENCE must be fresh; retained acceptance inputs cannot be overwritten' >&2
  exit 2
fi
mkdir -p "$M1_EVIDENCE/control" "$M1_EVIDENCE/frozen-bin" "$M1_EVIDENCE/browser-driver"
M1_EVIDENCE=$(realpath "$M1_EVIDENCE")
export M1_EVIDENCE
early_finish() {
  local status=$?
  printf '%s\n' "$status" > "$M1_EVIDENCE/exit-status"
  date -u +%FT%TZ > "$M1_EVIDENCE/finished-at"
}
trap early_finish EXIT
cp --preserve=mode,timestamps build/testing/m1_provenance.py "$M1_EVIDENCE/control/m1_provenance.py"
cp --preserve=mode,timestamps build/testing/source_snapshot.py "$M1_EVIDENCE/control/source_snapshot.py"
cp --preserve=mode,timestamps build/rust/isolated-libtest.py "$M1_EVIDENCE/control/isolated-libtest.py"
cp --preserve=mode,timestamps build/testing/run-m1-acceptance.sh "$M1_EVIDENCE/control/run-m1-acceptance.sh"
cp --preserve=mode,timestamps scripts/lib-extract.sh "$M1_EVIDENCE/control/lib-extract.sh"
cp --preserve=mode,timestamps "$TIDEPOOL_BROWSER_DRIVER" "$M1_EVIDENCE/browser-driver/driver.mjs"
ln -s "$(realpath "$M1_BROWSER_NODE_MODULES")" "$M1_EVIDENCE/browser-driver/node_modules"
cp --reflink=auto --preserve=mode,timestamps "$M1_FRONTEND" "$M1_EVIDENCE/frozen-bin/tidepool-extract"
cp --reflink=auto --preserve=mode,timestamps "$M1_WORKER" "$M1_EVIDENCE/frozen-bin/tidepool-extract-bin"
printf '%s  %s\n' "$M1_FRONTEND_SHA256" "$M1_EVIDENCE/frozen-bin/tidepool-extract" \
  "$M1_WORKER_SHA256" "$M1_EVIDENCE/frozen-bin/tidepool-extract-bin" > "$M1_EVIDENCE/producer-inputs.sha256"
sha256sum --check "$M1_EVIDENCE/producer-inputs.sha256"
python3 "$M1_EVIDENCE/control/m1_provenance.py" admit --root "$PWD" --output "$M1_EVIDENCE" > "$M1_EVIDENCE/provenance-admission.log" 2>&1 || {
  cat "$M1_EVIDENCE/provenance-admission.log" >&2
  exit 2
}

# Use the admitted immutable paths, rather than mutable aliases or Cargo config
# compiler overrides, for the actual campaign processes.
export RUSTC="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["tools"]["rustc"]["path"])' "$M1_EVIDENCE/provenance-admission.json")"
export RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER=
export TIDEPOOL_BROWSER_NODE="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["node"]["path"])' "$M1_EVIDENCE/provenance-admission.json")"
export PLAYWRIGHT_BROWSERS_PATH="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["browser"]["closure"])' "$M1_EVIDENCE/provenance-admission.json")"

python3 "$M1_EVIDENCE/control/source_snapshot.py" capture "$PWD" "$M1_EVIDENCE/source-before" --exclude test-source-boot
python3 "$M1_EVIDENCE/control/source_snapshot.py" audit "$PWD" "$M1_EVIDENCE/source-before" --output "$M1_EVIDENCE/source-before-compile-audit.json"
python3 "$M1_EVIDENCE/control/m1_provenance.py" bind-snapshot --root "$PWD" --output "$M1_EVIDENCE" > "$M1_EVIDENCE/worker-snapshot-binding.log" 2>&1 || {
  cat "$M1_EVIDENCE/worker-snapshot-binding.log" >&2
  exit 2
}
python3 - <<'PYCONTROL'
import hashlib,json,os
from pathlib import Path
p=Path(os.environ['M1_EVIDENCE'])
entries={entry['path']:entry for entry in json.loads((p/'source-before/manifest.json').read_text())['source']['entries']}
controls={'build/rust/isolated-libtest.py':'control/isolated-libtest.py',
          'build/testing/source_snapshot.py':'control/source_snapshot.py',
          'build/testing/m1_provenance.py':'control/m1_provenance.py',
          'build/testing/run-m1-acceptance.sh':'control/run-m1-acceptance.sh',
          'scripts/lib-extract.sh':'control/lib-extract.sh',
          'build/testing/browser/driver.mjs':'browser-driver/driver.mjs'}
for original,frozen in controls.items():
    assert entries[original]['sha256']==hashlib.sha256((p/frozen).read_bytes()).hexdigest(), original
PYCONTROL
sha256sum "$M1_EVIDENCE/control/isolated-libtest.py" "$M1_EVIDENCE/control/source_snapshot.py" \
  "$M1_EVIDENCE/control/run-m1-acceptance.sh" "$M1_EVIDENCE/control/m1_provenance.py" "$M1_EVIDENCE/control/lib-extract.sh" "$M1_EVIDENCE/browser-driver/driver.mjs" \
  "$M1_EVIDENCE/producer-provenance.json" > "$M1_EVIDENCE/control-inputs.sha256"

unset TIDEPOOL_ALLOW_STALE_EXTRACT TIDEPOOL_EXTRACT_DAEMON_SOCKET TIDEPOOL_EXTRACT_NO_DAEMON RUST_MIN_STACK
export TIDEPOOL_EXTRACT="$M1_EVIDENCE/frozen-bin/tidepool-extract"
export TIDEPOOL_EXTRACT_WORKER="$M1_EVIDENCE/frozen-bin/tidepool-extract-bin"
export TIDEPOOL_COMPILER_DEPLOYMENT="$M1_EVIDENCE/compiler-deployment.json"
export TIDEPOOL_PRELUDE_DIR="$PWD/bridge/haskell/lib"
export TIDEPOOL_GHC_LIBDIR="$(ghc --print-libdir)"
export TIDEPOOL_BROWSER_DRIVER="$M1_EVIDENCE/browser-driver/driver.mjs"
export TIDEPOOL_TEST_ARTIFACT_ROOT="$M1_EVIDENCE/battery"
export TIDEPOOL_DAEMON_ARGS='--workers 2 --rss-ceiling-mb 10240'
export TIDEPOOL_KEEP_TEST_LOGS=1 TIDEPOOL_EXTRACT_MEASUREMENT=1 TIDEPOOL_TIMING=1
export TIDEPOOL_TEST_COMPILER_TRACE_OUTPUT="$M1_EVIDENCE/compiler.raw.jsonl"

date -u +%FT%TZ > "$M1_EVIDENCE/compile-started-at"
cargo test -p tidepool --no-default-features --locked --offline --lib --no-run --message-format=json-render-diagnostics \
  > "$M1_EVIDENCE/compile.jsonl" 2> "$M1_EVIDENCE/compile.log"
date -u +%FT%TZ > "$M1_EVIDENCE/compile-finished-at"
python3 "$M1_EVIDENCE/control/source_snapshot.py" audit "$PWD" "$M1_EVIDENCE/source-before" --output "$M1_EVIDENCE/source-after-compile-audit.json"
python3 - <<'PYFREEZE'
import hashlib,json,os,subprocess
from pathlib import Path
p=Path(os.environ['M1_EVIDENCE']); artifacts=[]
for line in (p/'compile.jsonl').read_text().splitlines():
    try: record=json.loads(line)
    except ValueError: continue
    if record.get('reason')=='compiler-artifact' and record.get('executable') and record.get('target',{}).get('name')=='tidepool' and record.get('profile',{}).get('test'):
        artifacts.append(record)
assert len(artifacts)==1, len(artifacts)
record=artifacts[0]; dest=p/'facade-libtest'
subprocess.run(['cp','--reflink=auto','--preserve=mode,timestamps',record['executable'],str(dest)],check=True)
(p/'facade-artifact.json').write_text(json.dumps(record,indent=2)+'\n')
(p/'facade-libtest.sha256').write_text(hashlib.sha256(dest.read_bytes()).hexdigest()+'\n')
environment={key:value for key,value in os.environ.items() if key.startswith(('TIDEPOOL_','EXOMONAD_EMBEDDED_','PLAYWRIGHT_','M1_')) and not any(word in key for word in ('SECRET','TOKEN','KEY','AUTH'))}
(p/'environment.json').write_text(json.dumps(environment,sort_keys=True,indent=2)+'\n')
PYFREEZE
sha256sum "$M1_EVIDENCE/facade-libtest" > "$M1_EVIDENCE/facade-inputs.sha256"

source "$M1_EVIDENCE/control/lib-extract.sh"
resolve_tidepool_extract
prepare_battery_artifacts final-m1 bash "$M1_EVIDENCE/control/run-m1-acceptance.sh"
runner_pid=''
finish() {
  local status=$? final_status=$?
  if ! finalize_battery_artifacts "$status"; then final_status=1; fi
  teardown_battery_daemon
  if ! python3 "$M1_EVIDENCE/control/source_snapshot.py" audit "$PWD" "$M1_EVIDENCE/source-before" --output "$M1_EVIDENCE/source-after-tests-audit.json"; then final_status=1; fi
  if ! sha256sum --check "$M1_EVIDENCE/control-inputs.sha256" > "$M1_EVIDENCE/control-post-audit.log"; then final_status=1; fi
  if ! python3 "$M1_EVIDENCE/control/m1_provenance.py" audit --output "$M1_EVIDENCE" > "$M1_EVIDENCE/browser-inputs-post-audit.log" 2>&1; then final_status=1; fi
  if ! sha256sum --check "$M1_EVIDENCE/provenance-inputs.sha256" > "$M1_EVIDENCE/provenance-post-audit.log"; then final_status=1; fi
  if ! sha256sum --check "$M1_EVIDENCE/producer-inputs.sha256" > "$M1_EVIDENCE/producer-post-audit.log"; then final_status=1; fi
  if ! sha256sum --check "$M1_EVIDENCE/facade-inputs.sha256" > "$M1_EVIDENCE/facade-post-audit.log"; then final_status=1; fi
  sha256sum "$TIDEPOOL_EXTRACT" "$TIDEPOOL_EXTRACT_WORKER" > "$M1_EVIDENCE/producer-post.sha256"
  printf '%s\n' "$final_status" > "$M1_EVIDENCE/exit-status"
  date -u +%FT%TZ > "$M1_EVIDENCE/finished-at"
  exit "$final_status"
}
trap finish EXIT
trap '[ -z "$runner_pid" ] || _terminate_and_wait "$runner_pid" m1-runner; exit 130' INT TERM
date -u +%FT%TZ > "$M1_EVIDENCE/started-at"
start_battery_daemon
printf '%s\n' "$BATTERY_ARTIFACT_DIR" > "$M1_EVIDENCE/battery-path"
run_selection() {
  local label=$1; shift
  set +e
  python3 "$M1_EVIDENCE/control/isolated-libtest.py" "$M1_EVIDENCE/facade-libtest" --jobs 1 \
    --output-dir "$M1_EVIDENCE/retained-success-logs/$label" "$@" > "$M1_EVIDENCE/$label.log" 2>&1 &
  runner_pid=$!
  wait "$runner_pid"
  local status=$?
  runner_pid=''
  set -e
  printf '%s\n' "$status" > "$M1_EVIDENCE/$label.exit-status"
  cat "$M1_EVIDENCE/$label.log" >> "$BATTERY_NEXTEST_LOG"
  return "$status"
}
http_status=0
browser_status=0
run_selection http3 --timeout 600 --expected-count 3 \
  --exact actor_host::m1_host_tests::production_host_retains_http_haskell_commands_and_reconnects_without_replay \
  --exact actor_host::m1_host_tests::host_cancellation_stops_a_real_running_haskell_cell \
  --exact actor_host::m1_host_tests::production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure || http_status=$?
run_selection browser1 --timeout 900 --expected-count 1 --ignored \
  --exact actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root || browser_status=$?
printf 'HTTP3 status=%s Browser1 status=%s\n' "$http_status" "$browser_status"
if [ "$http_status" -ne 0 ] || [ "$browser_status" -ne 0 ]; then exit 1; fi
