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
