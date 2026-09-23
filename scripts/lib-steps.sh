# Independent verification steps. Source this file; do not execute it.
#
# A gate is a list of independent questions (does it format, does it lint,
# do the tests pass, are fixtures fresh). One step's failure must not hide the
# others' answers, so `run_step` records a failure and continues, and
# `finish_steps` reports every failed step and exits with the worst status.

overall_status=0
failed_steps=()

# run_step DESCRIPTION COMMAND [ARGS...]
run_step() {
  local desc="$1"
  shift
  echo "==> STEP: $desc"
  local status=0
  "$@" || status=$?
  if [[ "$status" -ne 0 ]]; then
    echo "==> FAILED: $desc (exit $status)"
    failed_steps+=("$desc")
    overall_status=1
  fi
}

# run_step_background DESCRIPTION COMMAND [ARGS...]: start an independent step
# whose output would interleave with a foreground step. Its output goes to a
# private log that `wait_background_steps` prints once it finishes, so each
# step's output stays contiguous. Only use for steps that share no mutable
# state with what runs meanwhile.
background_pids=()
background_descs=()
background_logs=()
run_step_background() {
  local desc="$1"
  shift
  local log
  log="$(mktemp -t tidepool-step.XXXXXX)"
  echo "==> STEP (background): $desc"
  "$@" >"$log" 2>&1 &
  background_pids+=("$!")
  background_descs+=("$desc")
  background_logs+=("$log")
}

# wait_background_steps: collect every background step, print its output, and
# record failures exactly as run_step does.
wait_background_steps() {
  local i status
  for i in "${!background_pids[@]}"; do
    status=0
    wait "${background_pids[$i]}" || status=$?
    echo "==> OUTPUT: ${background_descs[$i]}"
    cat "${background_logs[$i]}"
    rm -f "${background_logs[$i]}"
    if [[ "$status" -ne 0 ]]; then
      echo "==> FAILED: ${background_descs[$i]} (exit $status)"
      failed_steps+=("${background_descs[$i]}")
      overall_status=1
    fi
  done
  background_pids=()
  background_descs=()
  background_logs=()
}

# kill_background_steps: stop background steps on interrupt.
kill_background_steps() {
  local pid
  for pid in "${background_pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
}

# finish_steps LABEL: summarize and exit non-zero if any step failed.
finish_steps() {
  local label="$1"
  if [[ "$overall_status" -ne 0 ]]; then
    echo "$label: ${#failed_steps[@]} step(s) FAILED:"
    printf '  - %s\n' "${failed_steps[@]}"
    exit "$overall_status"
  fi
  echo "$label: all steps passed"
}
