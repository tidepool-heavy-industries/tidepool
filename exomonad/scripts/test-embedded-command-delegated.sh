#!/usr/bin/env bash
# Keep the compiler and libtest runner outside the fresh delegated test service.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
descriptor="${1:?usage: test-embedded-command-delegated.sh DESCRIPTOR OUTPUT}"
output="${2:?fresh evidence directory required}"
[[ $# -eq 2 ]] || { echo 'error: expected DESCRIPTOR OUTPUT' >&2; exit 2; }
[[ ! -e "$output" ]] || { echo "error: evidence directory already exists: $output" >&2; exit 2; }
mkdir -m 700 -p "$output"
source "$repo_root/scripts/lib-extract.sh"
select_native_bundle "$descriptor"
cleanup() {
  local status=$?
  teardown_compile_daemon --preserve-logs
  if [[ -n "$COMPILE_DAEMON_SOCKET_DIR" && -d "$COMPILE_DAEMON_SOCKET_DIR" ]]; then
    cp -a "$COMPILE_DAEMON_SOCKET_DIR" "$output/compiler"
    rm -rf "$COMPILE_DAEMON_SOCKET_DIR"
  fi
  return "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
start_compile_daemon
"$NATIVE_OPERATOR_PYTHON" "$(dirname -- "$descriptor")/isolated-libtest.py" "$TIDEPOOL_NATIVE_LIBTEST" \
  --exact actor_host::embedded_command_tests::embedded_host_hands_out_and_executes_the_resident_command_backend \
  --expected-count 1 --ignored --jobs 1 --delegated-service \
  --service-slice "${TIDEPOOL_TEST_SLICE:-app.slice}" --output-dir "$output/tests"
