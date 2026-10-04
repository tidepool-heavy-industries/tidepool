#!/usr/bin/env bash
# The declared runner owns discovery/counts and launches only each test process
# into a fresh delegated service, leaving Buck and the runner outside it.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
output="${1:?usage: test-command-resources-delegated.sh OUTPUT}"
[[ $# -eq 1 ]] || { echo 'error: expected a fresh evidence directory' >&2; exit 2; }
output="$(realpath -m -- "$output")"
[[ ! -e "$output" ]] || { echo "error: evidence directory already exists: $output" >&2; exit 2; }
exec bash "$repo_root/scripts/buck2-run.sh" run --local-only -c remote.enabled=false \
  //exomonad/node:command_resources -- \
  --exact command_oom_and_queue_preserve_the_control_process \
  --exact actor_admission_times_out_without_starting \
  --exact shared_clients_retain_queued_work_after_observer_disconnect \
  --expected-count 3 --ignored --jobs 1 --delegated-service \
  --service-slice "${TIDEPOOL_TEST_SLICE:-app.slice}" --output-dir "$output"
