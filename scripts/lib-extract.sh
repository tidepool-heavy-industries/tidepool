#!/usr/bin/env bash
# Shared TIDEPOOL_EXTRACT resolution + validation, sourced by battery.sh,
# battery-shard.sh, and bench-turn.sh. One copy so the three callers can't
# drift out of sync with each other.
#
# Usage: source this file, then call `resolve_tidepool_extract`. Callers must
# already be cd'd to the repo root (all three do this before sourcing) since
# the build step below runs `cd haskell`.
#
# Also owns the resident-compile-daemon lifecycle helpers
# (start_battery_daemon / teardown_battery_daemon) used by battery.sh and
# battery-shard.sh — see plans/compile-daemon-design.md §7 phase 1. Kept
# here, not duplicated per script, for the same reason as
# resolve_tidepool_extract above.

resolve_tidepool_extract() {
  # Captured BEFORE the build-if-unset branch below: the staleness check
  # after it only applies to a caller-SUPPLIED TIDEPOOL_EXTRACT — a binary
  # this function just built itself is trivially fresh (see
  # scripts/toolchain-doctor.sh, which runs the identical check standalone
  # and explains the nix-store mtime caveat this skips around).
  local _was_preset=0
  [ -n "${TIDEPOOL_EXTRACT:-}" ] && _was_preset=1

  if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
    echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
    # The locally-built binary needs the with-packages GHC (supplying lens/
    # freer-simple) on PATH at runtime, or extraction fails with "Could not
    # find module Control.Lens". The deployed nix wrapper hard-codes that
    # GHC's path; reuse it so a bare `nix develop` run works without manual
    # PATH surgery.
    _w="$HOME/.nix-profile/bin/tidepool-extract"
    if [ -x "$_w" ]; then
      _ghc="$(grep -oE '/nix/store/[^:"]*-with-packages/bin' "$_w" | head -1)"
      if [ -n "${_ghc:-}" ] && [ -d "$_ghc" ]; then
        export PATH="$_ghc:$PATH"
        echo "==> prepended with-packages GHC to PATH ($_ghc)"
      fi
    fi
    ( cd haskell && cabal build tidepool-extract-bin )
    # Split assignment from export: `export VAR="$(cmd)"` masks the command's
    # exit status (SC2155), so a failed list-bin would proceed with an empty
    # var.
    TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
    export TIDEPOOL_EXTRACT
  fi

  # A caller-supplied TIDEPOOL_EXTRACT skipped the build-above branch
  # entirely, so it never got the freshness this function gives its own
  # builds for free — this is the stale-ambient-extract class (bit three
  # times in one week): a binary that runs cleanly through the Usage: probe
  # below but was compiled before a subsequent haskell/{src,lib} edit, then
  # fails deep in a GHC-heavy test with an unrelated-looking error, or
  # silently exercises old translation logic. `nix` canonicalizes every store
  # path's mtime to a fixed near-epoch value for reproducibility (this covers
  # both a literal /nix/store/... path and the ~/.nix-profile/bin wrapper
  # symlink chain, whose resolved mtime is the same), so a binary whose mtime
  # predates the year 2000 is treated as "not applicable" rather than a false
  # STALE — see toolchain-doctor.sh's matching caveat for the full reasoning.
  if [ "$_was_preset" = 1 ] && [ -x "$TIDEPOOL_EXTRACT" ]; then
    _bin_mtime="$(stat -c %Y "$TIDEPOOL_EXTRACT" 2>/dev/null || echo 0)"
    _plausible_mtime_floor=946684800 # year 2000 — below this looks nix-canonicalized, not a real build time
    if [ "$_bin_mtime" -gt "$_plausible_mtime_floor" ]; then
      _newest_src="$(find "$PWD/haskell/src" "$PWD/haskell/lib" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      _newest_src="${_newest_src:-0}"
      if [ "$_bin_mtime" -lt "$_newest_src" ]; then
        if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" = "1" ]; then
          echo "warning: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than the newest haskell/{src,lib} file — continuing (TIDEPOOL_ALLOW_STALE_EXTRACT=1)" >&2
        else
          echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than the newest haskell/{src,lib} file — your extract binary is stale, rebuild from this worktree" >&2
          echo "  fix: cd haskell && cabal build tidepool-extract-bin && export TIDEPOOL_EXTRACT=\$(cd haskell && cabal list-bin tidepool-extract-bin)" >&2
          echo "  or, for a deliberate cross-worktree/pinned run: TIDEPOOL_ALLOW_STALE_EXTRACT=1" >&2
          echo "  (full diagnostic: scripts/toolchain-doctor.sh)" >&2
          exit 1
        fi
      fi
    fi
  fi

  # The announced binary must actually run. eval_harness::extract_env
  # silently falls back to a `cabal list-bin` binary when the announced one
  # doesn't execute (so the banner below could name a binary the tests never
  # used), and when nothing resolves the GHC-guarded suites "skip cleanly" —
  # a green battery with zero GHC coverage. Same no-args `Usage:` probe
  # extract_env uses — the banner is on stderr, written before stdout's
  # diagnostics JSON, so a plain merged `2>&1` (not a stdout/stderr swap)
  # sees it first either way. Do NOT truncate the read with `head -c N`: the
  # extract binary ALWAYS writes a second thing after the banner (the
  # diagnostics JSON, to stdout) — `head` closing the pipe the instant it has
  # its N bytes races that second write, and an EPIPE there is an uncaught
  # exception that fails the process (an intermittent nonzero exit with no
  # other symptom). `grep` alone drains the pipe to EOF, so the writer never
  # gets closed out from under it.
  if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
    exit 1
  fi
  echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
}

# --- Resident compile daemon (plans/compile-daemon-design.md §7 phase 1) ---
#
# Globals set by start_battery_daemon and read by teardown_battery_daemon:
#   BATTERY_DAEMON_PID         pid of the daemon THIS process started, or ""
#   BATTERY_DAEMON_SOCKET_DIR  per-run tempdir holding the socket+log, or ""
#   BATTERY_DAEMON_OWNED       1 iff this process owns the daemon's lifecycle
#                               (0 when reusing an outer wrapper's daemon, or
#                               when the daemon is disabled/failed to start)
BATTERY_DAEMON_PID=""
BATTERY_DAEMON_SOCKET_DIR=""
BATTERY_DAEMON_OWNED=0

# Best-effort liveness check for an inherited $TIDEPOOL_EXTRACT_DAEMON_SOCKET
# (outer-wrapper respect: a chain script that already started a daemon for
# many shard invocations must not get a second one started underneath it).
# `-S` alone only proves the path is a socket special file, which survives a
# crashed daemon — so also try a real connect via python3 (already used
# elsewhere in scripts/, e.g. bench-turn.sh) when it's on PATH; without
# python3, fall back to the `-S` check alone. Either way this is advisory:
# ExtractCmd::run() falls back to a direct spawn per request on any
# daemon-unavailable signal (tidepool-extract-cmd/CLAUDE.md), so a false
# "alive" verdict here costs a slower run, never correctness.
_battery_daemon_socket_alive() {
  local sock="$1"
  [ -S "$sock" ] || return 1
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$sock" >/dev/null 2>&1 <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(1)
try:
    s.connect(sys.argv[1])
except OSError:
    sys.exit(1)
else:
    s.close()
PY
    return $?
  fi
  return 0
}

# Regenerable cache root, mirroring tidepool-runtime::paths::cache_dir()
# (XDG_CACHE_HOME -> ~/.cache -> $TMPDIR, joined with "tidepool"). The ONE
# bash reimplementation of that precedence — scripts/current-run.sh sources
# this file and calls this function rather than keeping its own copy (root
# CLAUDE.md's "kept-in-sync copies are forbidden" rule).
cache_dir() {
  if [ -n "${XDG_CACHE_HOME:-}" ]; then
    echo "${XDG_CACHE_HOME}/tidepool"
  elif [ -n "${HOME:-}" ]; then
    echo "${HOME}/.cache/tidepool"
  else
    echo "${TMPDIR:-/tmp}/tidepool"
  fi
}

# Resolves the deploy-handshake toolchain stamp path the same way the
# servers do (tidepool-runtime::toolchain, haskell/CLAUDE.md's Deploy
# handshake section: <cache_dir>/toolchain-stamp.json, override
# $TIDEPOOL_TOOLCHAIN_STAMP) — not a second path-resolution mechanism, just
# this precedence expressed in bash, via the shared cache_dir() above.
_battery_daemon_stamp_path() {
  if [ -n "${TIDEPOOL_TOOLCHAIN_STAMP:-}" ]; then
    echo "$TIDEPOOL_TOOLCHAIN_STAMP"
    return
  fi
  echo "$(cache_dir)/toolchain-stamp.json"
}

# Starts a per-run resident compile daemon and exports
# TIDEPOOL_EXTRACT_DAEMON_SOCKET for the caller's whole nextest invocation.
# Must run after resolve_tidepool_extract (needs $TIDEPOOL_EXTRACT).
#
# ON BY DEFAULT (2026-08-24): the phase-1 blocker (warm-daemon spawnSpec
# memo poison) was fixed by content-validating memo hits against GHC's own
# ms_hs_hash (lookupValidMemo, GhcPipeline.hs), and the handlers A/B went
# 197/197 on both legs — daemon leg 45s vs 71s direct-spawn, cold isolated
# caches. TIDEPOOL_EXTRACT_NO_DAEMON=1 is the kill switch (checked first,
# unconditional). Measurements + history: plans/compile-daemon-design.md
# Phase 1 status.
#
# Outer-wrapper respect: if $TIDEPOOL_EXTRACT_DAEMON_SOCKET is already set
# and looks alive, reuse it and leave BATTERY_DAEMON_OWNED=0 — a chain
# invocation (e.g. battery-shard.sh runs launched back-to-back by another
# script that already started a daemon) must not start, or later tear down,
# a second one. Checked before the opt-in gate below: reusing an inherited,
# already-running daemon is always correct regardless of whether this
# particular invocation's own env re-states the opt-in.
#
# The daemon crashing or being unreachable mid-run needs no handling here:
# ExtractCmd::run() (the ONE tidepool-extract invocation builder,
# tidepool-extract-cmd/CLAUDE.md) already falls back to a direct spawn per
# request on any daemon-unavailable signal — connect failure, timeout, or a
# crash mid-request. This function only ever makes TIDEPOOL_EXTRACT_DAEMON_SOCKET
# available; it never becomes a requirement for the run to proceed.
start_battery_daemon() {
  BATTERY_DAEMON_PID=""
  BATTERY_DAEMON_SOCKET_DIR=""
  BATTERY_DAEMON_OWNED=0

  if [ "${TIDEPOOL_EXTRACT_NO_DAEMON:-0}" = "1" ]; then
    echo "==> TIDEPOOL_EXTRACT_NO_DAEMON=1 — skipping the resident compile daemon (direct spawn per request)" >&2
    return 0
  fi

  if [ -n "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ] && _battery_daemon_socket_alive "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"; then
    echo "==> reusing already-running compile daemon at $TIDEPOOL_EXTRACT_DAEMON_SOCKET (outer wrapper owns its lifecycle)" >&2
    return 0
  fi

  # Default ON (flipped 2026-08-24 after the spawnSpec memo-poison fix —
  # lookupValidMemo content-validation — took the handlers A/B to 197/197
  # both legs; see plans/compile-daemon-design.md's Phase 1 status).
  # TIDEPOOL_EXTRACT_NO_DAEMON=1 above is the kill switch.

  BATTERY_DAEMON_SOCKET_DIR="$(mktemp -d -t tidepool-extract-daemon.XXXXXX)"
  local sock="$BATTERY_DAEMON_SOCKET_DIR/extract.sock"
  local log="$BATTERY_DAEMON_SOCKET_DIR/daemon.log"

  local stamp
  stamp="$(_battery_daemon_stamp_path)"
  local watch_args=()
  if [ -f "$stamp" ]; then
    watch_args=(--watch-stamp "$stamp")
  else
    echo "==> no toolchain stamp at $stamp (nothing deployed via scripts/redeploy.sh on this machine yet) — starting compile daemon without --watch-stamp" >&2
  fi

  echo "==> starting per-run resident compile daemon: socket=$sock log=$log" >&2
  # Rotation/RSS-ceiling flags deliberately omitted — ride the binary's own
  # defaults (plans/compile-daemon-design.md Decisions item 3: N=256
  # requests, 2048MB RSS ceiling, both provisional and tuned there, not here).
  "$TIDEPOOL_EXTRACT" --daemon --socket "$sock" "${watch_args[@]}" >"$log" 2>&1 &
  BATTERY_DAEMON_PID=$!
  # Recorded before the boot-wait below so a signal arriving mid-wait still
  # tears this down correctly (the caller installs its cleanup trap before
  # calling this function).

  local waited=0
  while [ ! -S "$sock" ]; do
    if ! kill -0 "$BATTERY_DAEMON_PID" 2>/dev/null; then
      echo "==> compile daemon exited before it came up (see $log) — continuing without it" >&2
      BATTERY_DAEMON_PID=""
      return 0
    fi
    if [ "$waited" -ge 30 ]; then
      echo "==> compile daemon did not create its socket within 30s (see $log) — continuing without it" >&2
      kill -TERM "$BATTERY_DAEMON_PID" 2>/dev/null || true
      BATTERY_DAEMON_PID=""
      return 0
    fi
    sleep 0.5
    waited=$((waited + 1))
  done

  export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$sock"
  BATTERY_DAEMON_OWNED=1
  echo "==> compile daemon up: pid=$BATTERY_DAEMON_PID socket=$sock" >&2
}

# Sends TERM to pid $1, waits up to a 10s grace period (polling `kill -0`),
# escalates to KILL on that SAME pid if it's still alive, then blocks until
# it's actually reaped (`wait`) — so a caller never proceeds (e.g. releasing
# the ghc-slots.sh semaphore by exiting) while $1 or its own children may
# still be alive. $2 is a short label for the log lines. Still "exact
# recorded pid, never pattern-kill" — this only ever escalates signal
# strength on the SAME identified process, never widens the target.
#
# The escalation is load-bearing, not defensive fluff: observed in
# practice, a process can catch SIGTERM (it's in its signal mask) without
# exiting promptly while idle-blocked in a syscall (e.g. the compile
# daemon's own accept() loop — GHC's RTS defers signal handling to the next
# safe point, which an idle blocking accept() may not reach for a while).
# A plain TERM+wait can therefore hang the caller indefinitely; SIGKILL
# cannot be caught or deferred, so the bound is a hard guarantee. Shared by
# teardown_battery_daemon below and both battery scripts' on_signal, so this
# sequence has exactly one implementation.
_terminate_and_wait() {
  local pid="$1" label="$2"
  kill -0 "$pid" 2>/dev/null || return 0
  kill -TERM "$pid" 2>/dev/null || true
  local term_sent_at=$SECONDS
  while kill -0 "$pid" 2>/dev/null; do
    if [ $((SECONDS - term_sent_at)) -ge 10 ]; then
      echo "==> $label (pid $pid) still alive 10s after SIGTERM — escalating to SIGKILL" >&2
      kill -KILL "$pid" 2>/dev/null || true
      break
    fi
    sleep 0.5
  done
  wait "$pid" 2>/dev/null || true
}

# Tears down a daemon this process started (no-op if BATTERY_DAEMON_OWNED=0
# — disabled, failed to start, or reusing an outer wrapper's daemon), via
# _terminate_and_wait above. Call from an EXIT trap installed BEFORE
# start_battery_daemon runs, so it also fires if a signal lands mid-boot
# (see start_battery_daemon's comment).
teardown_battery_daemon() {
  if [ "$BATTERY_DAEMON_OWNED" = 1 ] && [ -n "$BATTERY_DAEMON_PID" ]; then
    _terminate_and_wait "$BATTERY_DAEMON_PID" "compile daemon"
    echo "==> compile daemon (pid $BATTERY_DAEMON_PID) torn down" >&2
  fi
  BATTERY_DAEMON_PID=""
  if [ -n "$BATTERY_DAEMON_SOCKET_DIR" ] && [ -d "$BATTERY_DAEMON_SOCKET_DIR" ]; then
    rm -rf "$BATTERY_DAEMON_SOCKET_DIR"
  fi
  BATTERY_DAEMON_SOCKET_DIR=""
  BATTERY_DAEMON_OWNED=0
}
