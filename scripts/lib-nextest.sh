# Shared Nextest execution gate. Callers own signal traps and cleanup; the
# exported PID is the actual Cargo process so cleanup never loses its child.
nextest_pid=""
nextest_run_checked() {
  local log status remove_log=0
  log="${NEXTTEST_STDERR_LOG:-$(mktemp -t tidepool-nextest.XXXXXX)}"
  [[ -n "${NEXTTEST_STDERR_LOG:-}" ]] || remove_log=1
  set +e
  cargo nextest run "$@" 2> >(tee "$log" >&2) &
  nextest_pid=$!
  wait "$nextest_pid"
  status=$?
  nextest_pid=""
  set -e
  if (( status == 0 )); then
    if ! grep -Eq 'Summary.*[[:space:]][0-9]+ tests? run([:[:space:]]|$)' "$log"; then
      echo "error: Nextest completed without a test execution summary" >&2
      status=1
    elif grep -Eq 'Summary.*[[:space:]]0 tests? run([:[:space:]]|$)' "$log"; then
      echo "error: Nextest selected/ran ZERO tests — not a passing check" >&2
      status=1
    fi
  fi
  (( remove_log == 0 )) || rm -f "$log"
  return "$status"
}
