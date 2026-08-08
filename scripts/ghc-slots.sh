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
# Properties: single-slot waiters hold nothing while blocked (no deadlock).
# When all slots are busy, a single-slot waiter POLLS ALL slots with jitter
# rather than blocking untimed on one: committing to a single slot strands
# waiters while another slot sits free (observed 2026-08-09 — three waiters
# PID-hashed onto the same slot, two slots idle). With every waiter polling,
# no protocol class holds kernel-queue priority over another, so the old
# timed-retry starvation argument no longer applies; grant order among pollers
# is random but capacity is never wasted. `exclusive` still blocks untimed per
# slot in canonical order, so it out-queues pollers on each slot as it drains —
# which is what a measurement wants.
set -euo pipefail

SLOTS=(/tmp/tidepool-ghc.lock /tmp/tidepool-ghc.slot1 /tmp/tidepool-ghc.slot2 /tmp/tidepool-ghc.slot3)

# Memory gate: a GHC extract needs ~1-2Gi, so granting a slot when the box is
# already near-empty is how a burst tips into swap-thrash. Before taking a slot,
# wait until MemAvailable clears the floor. The waiters that are already RUNNING
# finish and free memory regardless of this gate — nothing they need is blocked
# here — so it drains rather than deadlocks. Override the floor with
# TIDEPOOL_GHC_MEM_FLOOR_MB (0 disables). This is a soft guard; a cgroup
# MemoryMax is the hard ceiling.
MEM_FLOOR_MB="${TIDEPOOL_GHC_MEM_FLOOR_MB:-6144}"
mem_available_mb() { awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo; }
await_memory() {
  [ "$MEM_FLOOR_MB" -gt 0 ] || return 0
  local waited=0
  while [ "$(mem_available_mb)" -lt "$MEM_FLOOR_MB" ]; do
    if [ "$waited" = 0 ]; then
      echo "ghc-slots: MemAvailable $(mem_available_mb)MB < ${MEM_FLOOR_MB}MB floor — waiting for memory before taking a slot" >&2
    fi
    sleep 5
    waited=$((waited + 5))
  done
}

mode="${1:-}"
shift || true
[ "${1:-}" = "--" ] && shift
if [ -z "$mode" ] || [ $# -eq 0 ]; then
  echo "usage: ghc-slots.sh run|exclusive -- <cmd...>" >&2
  exit 2
fi

case "$mode" in
  run)
    await_memory
    announced=0
    while :; do
      for f in "${SLOTS[@]}"; do
        exec {fd}>"$f"
        if flock -n "$fd"; then
          await_memory
          # Marker for scripts that self-slot (scripts/battery.sh,
          # scripts/battery-shard.sh): held here, so they must not acquire a
          # second one. Set only after the slot is actually taken.
          export TIDEPOOL_GHC_SLOT="$f"
          exec "$@"
        fi
        exec {fd}>&-
      done
      if [ "$announced" = 0 ]; then
        echo "ghc-slots: all slots busy — polling for any free slot" >&2
        announced=1
      fi
      sleep $((5 + RANDOM % 10))
    done
    ;;
  exclusive)
    await_memory
    for f in "${SLOTS[@]}"; do
      exec {fd}>"$f"
      flock "$fd"
    done
    export TIDEPOOL_GHC_SLOT="all"
    exec "$@"
    ;;
  *)
    echo "unknown mode: $mode (want run|exclusive)" >&2
    exit 2
    ;;
esac
