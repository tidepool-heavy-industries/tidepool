#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ ${TIDEPOOL_DEV_SHELL:-} != *#default ]]; then
  exec /run/current-system/sw/bin/bash \
    "$repo_root/scripts/dev-shell.sh" \
    "$repo_root/exomonad/scripts/test-embedded-command-delegated.sh"
fi
cd "$repo_root"

test_name="actor_host::embedded_command_tests::embedded_host_hands_out_and_executes_the_resident_command_backend"
test_slice="${TIDEPOOL_TEST_SLICE:-app.slice}"
unit="tidepool-embedded-command-test-$$"
artifact_root="${TIDEPOOL_TEST_ARTIFACT_ROOT:-$repo_root/target/tidepool-test-runs}"
mkdir -p "$artifact_root"
artifact_dir="$(mktemp -d "$artifact_root/embedded-command.XXXXXX")"
build_log="$artifact_dir/build.log"
build_json="$artifact_dir/cargo.jsonl"
test_log="$artifact_dir/test.log"

source scripts/lib-extract.sh
BATTERY_DAEMON_PID=""
BATTERY_DAEMON_SOCKET_DIR=""
BATTERY_DAEMON_OWNED=0
BATTERY_DAEMON_START_FAILED=0
BATTERY_ARTIFACT_DIR=""
cleanup() {
  local status=$?
  teardown_battery_daemon
  if [[ $status -eq 0 && ${TIDEPOOL_KEEP_TEST_LOGS:-1} != 1 ]]; then
    rm -rf "$artifact_dir"
  else
    echo "embedded command gate artifacts: $artifact_dir" >&2
  fi
  return "$status"
}
trap cleanup EXIT

# Resolve this checkout's frontend/worker and keep their per-run compiler
# daemon outside the delegated service. The service then contains only the
# focused test process, which needs an empty delegated cgroup to create its
# command resource owner.
resolve_tidepool_extract --prefer-persistent-daemon
start_battery_daemon

if ! cargo test -p tidepool --lib --no-default-features --no-run \
  --message-format=json >"$build_json" 2>"$build_log"; then
  cat "$build_log" >&2
  echo "test binary preparation failed; artifacts: $artifact_dir" >&2
  exit 1
fi
binary="$(jq -r '
  select(
    .reason == "compiler-artifact"
    and .profile.test == true
    and .target.name == "tidepool"
  )
  | .executable // empty
' "$build_json" | tail -n 1)"
if [[ -z "$binary" || ! -x "$binary" ]]; then
  echo "could not locate the no-default-features tidepool library test binary" >&2
  echo "Cargo output retained at $artifact_dir" >&2
  exit 1
fi

env_args=(
  --setenv="PATH=$PATH"
  --setenv="HOME=$HOME"
  --setenv="TIDEPOOL_KEEP_TEST_LOGS=${TIDEPOOL_KEEP_TEST_LOGS:-1}"
)
for name in \
  CARGO_TARGET_DIR TIDEPOOL_DEV_FLAKE TIDEPOOL_DEV_SHELL \
  TIDEPOOL_DEV_GHC TIDEPOOL_DEV_RUSTC GHC_LIBDIR TIDEPOOL_EXTRACT \
  TIDEPOOL_EXTRACT_WORKER TIDEPOOL_EXTRACT_SOURCES \
  TIDEPOOL_EXTRACT_DAEMON_SOCKET TIDEPOOL_EXTRACT_DAEMON_LOG; do
  if [[ -n ${!name:-} ]]; then
    env_args+=(--setenv="$name=${!name}")
  fi
done

set +e
systemd-run \
  --user \
  --pipe \
  --wait \
  --collect \
  --service-type=exec \
  --property=Delegate=yes \
  --slice="$test_slice" \
  --working-directory="$repo_root" \
  --unit="$unit" \
  "${env_args[@]}" \
  "$(realpath "$binary")" \
  --exact "$test_name" \
  --ignored \
  --nocapture >"$test_log" 2>&1
status=$?
set -e
cat "$test_log"
if [[ $status -ne 0 ]]; then
  echo "delegated test failed; artifacts: $artifact_dir" >&2
  exit "$status"
fi
if ! grep -Fxq 'running 1 test' "$test_log" \
  || ! grep -Eq '^test result: ok\. 1 passed; 0 failed;' "$test_log"; then
  echo "focused binary did not execute exactly one passing test; artifacts: $artifact_dir" >&2
  exit 1
fi
