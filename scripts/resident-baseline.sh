#!/usr/bin/env bash
# Attribute complete-cell compilation and real durable publication through the
# existing battery launcher. This is not the packaged Engine/Store latency gate.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# != 1 || "$1" != /* ]]; then
  echo "usage: scripts/resident-baseline.sh ABSOLUTE_NEW_EVIDENCE_DIRECTORY" >&2
  exit 2
fi
: "${TIDEPOOL_EXTRACT:?select a frozen matched frontend}"
: "${TIDEPOOL_EXTRACT_WORKER:?select a frozen matched worker}"
: "${TIDEPOOL_COMPILER_DEPLOYMENT:?select its configured deployment manifest}"
: "${CARGO_TARGET_DIR:?select the admitted checkout target directory}"
evidence="$1"
mkdir "$evidence"
export TIDEPOOL_DAEMON_ARGS='--workers 2 --rss-ceiling-mb 10240'
export TIDEPOOL_TIMING=1 TIDEPOOL_KEEP_TEST_LOGS=1
export TIDEPOOL_TEST_ARTIFACT_ROOT="$evidence/battery"
export TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT="$evidence/workspaces"
mkdir "$TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT"
unset TIDEPOOL_EXTRACT_NO_DAEMON TIDEPOOL_EXTRACT_DAEMON_SOCKET
filter='test(=session::turn::scaling_tests::complete_cell_consumes_item_and_display_without_compiler_requests) | test(=session::turn::scaling_tests::resident_durable_display_cells_2_baseline)'
command=(bash scripts/battery.sh -p tidepool-runtime --lib --run-ignored all --test-threads 1 --success-output immediate --failure-output immediate -E "$filter")
python3 - "$evidence/manifest.json" "${command[@]}" <<'PY'
import hashlib, json, os, pathlib, subprocess, sys
selected = {}
for name in ('TIDEPOOL_EXTRACT', 'TIDEPOOL_EXTRACT_WORKER', 'TIDEPOOL_COMPILER_DEPLOYMENT'):
    path = pathlib.Path(os.environ[name])
    if not path.is_absolute() or not path.is_file():
        raise SystemExit(f'{name} must select a retained absolute file')
    selected[name] = {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
deployment = json.loads(pathlib.Path(os.environ['TIDEPOOL_COMPILER_DEPLOYMENT']).read_text())
for name, key in [('TIDEPOOL_EXTRACT', 'frontend_path'), ('TIDEPOOL_EXTRACT_WORKER', 'worker_path')]:
    if selected[name]['path'] != deployment.get(key):
        raise SystemExit(f'{name} differs from configured deployment')
manifest = {
    'schema': 1, 'composition': 'private-session-and-durable-publication',
    'source_oid': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
    'source_status': subprocess.check_output(['git', 'status', '--porcelain'], text=True).splitlines(),
    'command': sys.argv[2:], 'selected_files': selected, 'deployment': deployment,
    'profile': 'Cargo test (repository defaults)',
    'daemon_args': os.environ['TIDEPOOL_DAEMON_ARGS'],
    'cargo_target_dir': os.environ['CARGO_TARGET_DIR'],
}
pathlib.Path(sys.argv[1]).write_text(json.dumps(manifest, indent=2) + '\n')
PY
set +e
"${command[@]}" 2>&1 | tee "$evidence/baseline.log"
status=${PIPESTATUS[0]}
set -e
python3 - "$evidence/manifest.json" "$status" <<'PY'
import hashlib, json, pathlib, re, sys
path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest['battery_exit_code'] = int(sys.argv[2])
manifest['exit_code'] = manifest['battery_exit_code']
text = re.sub(r'\x1b\[[0-9;]*m', '', (path.parent / 'baseline.log').read_text())
summaries = re.findall(r'^.*Summary[^\n]*\b(\d+) tests run:', text, re.MULTILINE)
manifest['expected_test_count'] = 2
manifest['executed_test_count'] = int(summaries[-1]) if summaries else None
manifest['test_count_matches'] = manifest['executed_test_count'] == manifest['expected_test_count']
if not manifest['test_count_matches']:
    manifest['exit_code'] = 1
manifest['selected_files_unchanged'] = all(hashlib.sha256(pathlib.Path(row['path']).read_bytes()).hexdigest() == row['sha256'] for row in manifest['selected_files'].values())
try:
    cgroup = next(line.split(':', 2)[2] for line in pathlib.Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    manifest['scope_cgroup'] = cgroup
    manifest['scope_memory_peak_bytes'] = int((pathlib.Path('/sys/fs/cgroup') / cgroup.lstrip('/') / 'memory.peak').read_text())
except (OSError, StopIteration, ValueError):
    manifest['scope_memory_peak_bytes'] = None
if not manifest['selected_files_unchanged']:
    manifest['exit_code'] = 1
path.write_text(json.dumps(manifest, indent=2) + '\n')
if not manifest['selected_files_unchanged']:
    raise SystemExit('frozen compiler files changed during baseline')
if not manifest['test_count_matches']:
    raise SystemExit('resident baseline must execute exactly two tests')
PY
exit "$status"
