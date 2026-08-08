#!/usr/bin/env bash
# Counted-semaphore lock for GHC-heavy work (extract builds, --ignore-default-filter
# nextest runs). Replaces bare `flock /tmp/tidepool-ghc.lock <cmd>`.
#
#   scripts/ghc-slots.sh run -- <cmd...>        acquire ONE of N slots, run, release
#   scripts/ghc-slots.sh exclusive -- <cmd...>  acquire ALL slots (whole-box quiet:
#                                               latency measurement, benchmarks)
#
# Slot 0 is the legacy lock file, so processes still using the old single-lock
# protocol occupy slot 0 and total concurrency stays bounded during migration.
#
# Properties: single-slot waiters hold nothing while blocked (no deadlock);
# exclusive acquires slots in canonical order (no deadlock between exclusives)
# and has drain semantics — under continuous single-slot demand it waits for a
# lull, which is what a measurement wants anyway. When all slots are busy, a
# single-slot waiter blocks with NO timeout on a PID-spread slot (flock is not
# FIFO-fair; timed retry loops starve).
set -euo pipefail

SLOTS=(/tmp/tidepool-ghc.lock /tmp/tidepool-ghc.slot1 /tmp/tidepool-ghc.slot2)

mode="${1:-}"
shift || true
[ "${1:-}" = "--" ] && shift
if [ -z "$mode" ] || [ $# -eq 0 ]; then
  echo "usage: ghc-slots.sh run|exclusive -- <cmd...>" >&2
  exit 2
fi

case "$mode" in
  run)
    for f in "${SLOTS[@]}"; do
      exec {fd}>"$f"
      if flock -n "$fd"; then
        exec "$@"
      fi
      exec {fd}>&-
    done
    f="${SLOTS[$(($$ % ${#SLOTS[@]}))]}"
    exec {fd}>"$f"
    flock "$fd"
    exec "$@"
    ;;
  exclusive)
    for f in "${SLOTS[@]}"; do
      exec {fd}>"$f"
      flock "$fd"
    done
    exec "$@"
    ;;
  *)
    echo "unknown mode: $mode (want run|exclusive)" >&2
    exit 2
    ;;
esac
