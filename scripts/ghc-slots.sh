#!/usr/bin/env bash
# Counted-semaphore lock for GHC-heavy work (extract builds, --ignore-default-filter
# nextest runs). Replaces bare `flock /tmp/tidepool-ghc.lock <cmd>`.
#
#   scripts/ghc-slots.sh run -- <cmd...>        acquire ONE of N slots, run, release
#   scripts/ghc-slots.sh detach -- <cmd...>     same, but in its OWN SESSION via
#                                               setsid: the queue wait is not charged
#                                               against the caller's process lifetime
#                                               (the environment kills processes at
#                                               ~380s — a queued `run` under contention
#                                               dies WHILE WAITING; detach survives,
#                                               acquires, runs, and releases even if
#                                               the calling pane dies). Prints pid +
#                                               log path and returns immediately;
#                                               poll the log across turns.
#   scripts/ghc-slots.sh exclusive -- <cmd...>  acquire ALL slots (whole-box quiet:
#                                               latency measurement, benchmarks)
#
# Slot 0 is the legacy lock file, so processes still using the old single-lock
# protocol occupy slot 0 and total concurrency stays bounded during migration.
#
# Properties: single-slot waiters hold nothing while blocked (no deadlock).
# When all slots are busy, a waiter ROTATES over the slots, kernel-blocking on
# each with a jittered timeout (flock -w) rather than sleep-polling: blocked
# waiters cost ~zero CPU (43 sleep-pollers were measured burning 1.2 cores,
# 2026-08-08), and the rotation preserves the no-stranding property (a waiter
# committed untimed to one slot can starve while another sits free — observed
# 2026-08-09). Jittered timeouts keep waiters from cycling in lockstep;
# capacity is never wasted for longer than one timeout. `exclusive` still
# blocks untimed per slot in canonical order, so it out-queues rotating
# waiters on each slot as it drains — which is what a measurement wants.
set -euo pipefail

# THE COPY IN FORCE is the parent repo's, invoked by absolute path
# (/home/inanna/dev/tidepool/scripts/ghc-slots.sh) — a worktree's own copy of
# this file is INERT and its SLOTS= line may be stale; never audit slot count
# from a worktree checkout, and NEVER INVOKE a worktree copy: a stale copy
# that still sleep-polls is structurally STARVED against kernel-queued
# flock -w waiters (observed: 26 min queued, zero acquisitions). Outer-wrap
# with this absolute path; a worktree battery.sh inside inherits the slot
# marker and skips self-acquire. (The nextest cap below is the opposite:
# per-worktree config, live in each checkout.)
#
# 6-slot semaphore. THE REAL CEILING IS slots x nextest's per-run ghc-heavy
# cap (.config/nextest.toml) — the load-92 incident (2026-08-08) reached 7+
# concurrent extracts with every lane compliant at 3x3. Per-run cap is 1
# (fc3363dc, fleet-verified), so slots = box-wide extract ceiling. 6x1=6,
# still under the old effective 9. Six rather than four because cap-1's
# second-order cost is HOLD TIME: an --ignore-default-filter crate run
# serialises internally and holds one slot for its whole duration (observed
# 1h43m) while extracting only part of the time — so at equal ceiling, more
# slots means less head-of-line blocking behind long holds, not more load
# (a held slot is not a running extract; observed 3-6 extracts across 4
# held). Keep the product <= ~6 on this box; chase per-worktree cap
# stragglers rather than lowering this.
SLOTS=(/tmp/tidepool-ghc.lock /tmp/tidepool-ghc.slot1 /tmp/tidepool-ghc.slot2 /tmp/tidepool-ghc.slot3 /tmp/tidepool-ghc.slot4 /tmp/tidepool-ghc.slot5)

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
  echo "usage: ghc-slots.sh run|detach|exclusive -- <cmd...>" >&2
  exit 2
fi

case "$mode" in
  run)
    await_memory
    announced=0
    while :; do
      for f in "${SLOTS[@]}"; do
        exec {fd}>"$f"
        # First pass: non-blocking sweep grabs any free slot immediately.
        # Busy pass: kernel-block up to a jittered timeout, then rotate.
        if flock -n "$fd" || { [ "$announced" = 1 ] && flock -w $((10 + RANDOM % 10)) "$fd"; }; then
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
        echo "ghc-slots: all slots busy — blocking for any free slot" >&2
        announced=1
      fi
    done
    ;;
  detach)
    # New session so a process-group-scoped kill of the caller cannot reach the
    # queued waiter. The detached child is `run` itself: it holds the flock fd
    # once acquired and releases on exit, so slot discipline is fully honored.
    log="${TIDEPOOL_GHC_DETACH_LOG:-$(mktemp /tmp/tidepool-ghc-detach.XXXXXX.log)}"
    setsid nohup "$0" run -- "$@" >"$log" 2>&1 </dev/null &
    echo "ghc-slots: detached pid=$! log=$log"
    echo "$!"
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
