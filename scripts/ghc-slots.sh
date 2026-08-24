#!/usr/bin/env bash
# Counted-semaphore lock for GHC-heavy TEST work (--ignore-default-filter
# nextest runs and anything that FANS OUT extract compiles). Replaces bare
# `flock /tmp/tidepool-ghc.lock <cmd>`.
#
# TOOLCHAIN BUILDS ARE OUT OF SCOPE (reclassified 2026-08-09): a
# `cabal build tidepool-extract-bin` is ONE bounded GHC chain — run it
# UNBROKERED as `nice -n 15 cabal build -j4 ...` (the same envelope as
# pure-Rust cargo work), at most one per lane. The queue exists to cap
# extract FAN-OUT; a 3-minute build queuing behind an 88-minute suite was
# the V2 priority inversion surviving one category over. Long suite runs
# should SHARD their acquisitions (-E per binary/group, battery-shard
# discipline) so no single hold runs to an hour where receipts allow.
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
#                                               GUARANTEE BOUND: detach survives
#                                               the CALLING PANE dying (setsid).
#                                               It does NOT survive box-level
#                                               kill events (observed 2026-08-09:
#                                               an overnight event took every
#                                               detached waiter and job with it).
#                                               A detached job is not durable —
#                                               on any long gap, verify the job
#                                               actually ran (log start/exit
#                                               markers) before trusting a
#                                               conclusion built on it.
#                                               ENVELOPE: detach is about surviving
#                                               the WAIT, not running legs in
#                                               PARALLEL — at most ONE brokered leg
#                                               per dev at a time. Detach without
#                                               that bound is WORSE than no detach:
#                                               it converts the death-and-relaunch
#                                               that capped a non-adopter's
#                                               footprint into durable simultaneous
#                                               holds (observed: one dev holding
#                                               2 of 4 slots). Drain, don't kill:
#                                               a killed GHC leg wastes the slot
#                                               time already spent and frees the
#                                               slot no sooner; kill only work
#                                               that is known-void.
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
# 2-slot semaphore. THE REAL CEILING IS slots x nextest's per-run ghc-heavy
# cap (.config/nextest.toml) — that product is the box-wide concurrent-extract
# budget. Per-run cap is 4, so the accepted ceiling is 2x4=8 concurrent
# extracts (operator, 2026-08-24): the 6x4=24 window the old six-slot array
# allowed filled swap at 12 coincident ~700MB extracts and degraded the box.
# Each extract is a full GHC boot at ~600-800MB resident; 8 is what the RAM
# honestly supports. The second-order cost of fewer slots is HOLD TIME
# (head-of-line blocking behind a long --ignore-default-filter run that holds
# a slot for its whole duration) — accepted deliberately: throughput behind
# the semaphore beats another swap incident. Revisit once the resident
# compile daemon serves battery compiles (plans/compile-daemon-design.md):
# a per-run daemon serialises its own compiles, making the slot a
# fallback-path guard rather than the primary throttle. Raise either factor
# only with a fresh measurement — the product is the budget.
SLOTS=(/tmp/tidepool-ghc.lock /tmp/tidepool-ghc.slot1)

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

# Pre-start the sccache server (rustc-wrapper in ~/.cargo/config.toml) with
# CLEAN fds, BEFORE any slot flock is taken. If the first cargo build under a
# held slot starts it instead, the daemon inherits the wrapper shell's open
# fds — including the slot's flock fd and the command's stdout pipe — and
# holds both for its multi-hour idle lifetime: the slot reads as held by a
# dead pid, and pipeline readers (`... | tail`) never see EOF (observed
# 2026-08-19: one wedged battery + one wedged slot, both traced to a
# daemonized sccache via /proc/locks + /proc/*/fd). Idempotent and ~free when
# the server is already up; harmless if sccache is not installed.
command -v sccache >/dev/null 2>&1 && sccache --start-server </dev/null >/dev/null 2>&1 || true

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
          # Start/exit markers instead of bare exec: an environment failure
          # (e.g. ghc missing from the CALLER's PATH — hits run and detach
          # alike) dies in milliseconds, and without markers that log is
          # indistinguishable from a leg that ran. rc + duration make
          # "never started" / "died instantly" / "ran" mechanically
          # distinguishable in every log. The wrapper shell holds the flock
          # fd until the command finishes, so slot discipline is unchanged.
          echo "ghc-slots: acquired ${f##*.}, starting: $*" >&2
          start_s=$SECONDS
          "$@"
          rc=$?
          echo "ghc-slots: command exited rc=$rc after $((SECONDS - start_s))s" >&2
          exit "$rc"
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
