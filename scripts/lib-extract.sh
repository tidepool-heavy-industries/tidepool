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
# battery-shard.sh. Kept here, not duplicated per script, for the same reason as
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
    echo "==> TIDEPOOL_EXTRACT not set — building the Rust frontend and Haskell worker"
    # The worker loads Tidepool modules at runtime, so it needs the repository
    # with-packages compiler rather than a bare GHC. The Just recipes enter the
    # Nix shell that provides it; refuse an incomplete ambient shell instead of
    # scraping a separately installed extractor wrapper for a store path.
    if ! command -v ghc-pkg >/dev/null 2>&1 \
      || ! ghc-pkg list 2>/dev/null | grep -qE '\blens-[0-9]'; then
      echo "error: the active GHC does not expose lens; run through 'just' or enter 'nix develop'" >&2
      exit 1
    fi
    ( cd haskell && cabal build tidepool-extract-bin )
    cargo build -p tidepool-extract-cmd --bin tidepool-extract
    # Split assignment from export: `export VAR="$(cmd)"` masks the command's
    # exit status (SC2155), so a failed list-bin would proceed with an empty
    # var.
    TIDEPOOL_EXTRACT_WORKER="$(cd haskell && cabal list-bin tidepool-extract-bin)"
    TIDEPOOL_EXTRACT="$PWD/target/debug/tidepool-extract"
    export TIDEPOOL_EXTRACT TIDEPOOL_EXTRACT_WORKER
  fi

  # Check caller-supplied worktree binaries against the sources that build
  # each half. Nix store timestamps are canonicalized near the epoch, so they
  # cannot support this mtime check.
  if [ "$_was_preset" = 1 ] && [ -x "$TIDEPOOL_EXTRACT" ]; then
    _bin_mtime="$(stat -c %Y "$TIDEPOOL_EXTRACT" 2>/dev/null || echo 0)"
    _plausible_mtime_floor=946684800
    if [ "$_bin_mtime" -gt "$_plausible_mtime_floor" ]; then
      _newest_src="$(find "$PWD/tidepool-extract-cmd/src" "$PWD/tidepool-extract-cmd/Cargo.toml" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      _newest_src="${_newest_src:-0}"
      if [ "$_bin_mtime" -lt "$_newest_src" ]; then
        if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" = "1" ]; then
          echo "warning: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than tidepool-extract-cmd sources — continuing (TIDEPOOL_ALLOW_STALE_EXTRACT=1)" >&2
        else
          echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than tidepool-extract-cmd sources" >&2
          echo "  fix: cargo build -p tidepool-extract-cmd --bin tidepool-extract; cd haskell && cabal build tidepool-extract-bin" >&2
          echo "  or, for a deliberate cross-worktree/pinned run: TIDEPOOL_ALLOW_STALE_EXTRACT=1" >&2
          echo "  (full diagnostic: scripts/toolchain-doctor.sh)" >&2
          exit 1
        fi
      fi
    fi
  fi

  # A worktree frontend must resolve an executable compiler worker. Installed
  # packages provide a sibling worker through their wrapper; local builds set
  # this explicitly so the public/frontend and compiler halves cannot drift.
  if [ -n "${TIDEPOOL_EXTRACT_WORKER:-}" ] && [ ! -x "$TIDEPOOL_EXTRACT_WORKER" ]; then
    echo "error: TIDEPOOL_EXTRACT_WORKER='$TIDEPOOL_EXTRACT_WORKER' is not executable" >&2
    exit 1
  fi

  if [ -n "${TIDEPOOL_EXTRACT_WORKER:-}" ] && [ -x "$TIDEPOOL_EXTRACT_WORKER" ]; then
    _worker_mtime="$(stat -c %Y "$TIDEPOOL_EXTRACT_WORKER" 2>/dev/null || echo 0)"
    if [ "$_worker_mtime" -gt 946684800 ]; then
      _newest_haskell="$(find "$PWD/haskell/src" "$PWD/haskell/app" "$PWD/haskell/lib" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      if [ "$_worker_mtime" -lt "${_newest_haskell:-0}" ] && [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" != "1" ]; then
        echo "error: TIDEPOOL_EXTRACT_WORKER='$TIDEPOOL_EXTRACT_WORKER' is older than Haskell worker sources" >&2
        exit 1
      fi
    fi
  fi

  # Capture the whole no-args probe before searching it so an early-closing
  # pipe cannot give the frontend EPIPE while it writes diagnostics JSON.
  if [ ! -x "$TIDEPOOL_EXTRACT" ]; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not executable" >&2
    exit 1
  fi
  _usage_output="$("$TIDEPOOL_EXTRACT" 2>&1)"
  if ! grep -q '^Usage:' <<<"$_usage_output"; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
    exit 1
  fi
  echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
}

# --- Resident compile daemon ---
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
BATTERY_ARTIFACT_DIR=""
BATTERY_NEXTEST_LOG=""

# Failure artifacts for battery entry points. Successful runs leave nothing;
# failed runs retain the exact command, nextest output, toolchain report, and
# compile-daemon log under target/tidepool-test-runs/.
prepare_battery_artifacts() {
  local label="$1"
  shift
  local root="${TIDEPOOL_TEST_ARTIFACT_ROOT:-$PWD/target/tidepool-test-runs}"
  local stamp
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  BATTERY_ARTIFACT_DIR="$root/$stamp-$$-$label"
  BATTERY_NEXTEST_LOG="$BATTERY_ARTIFACT_DIR/nextest.log"
  mkdir -p "$BATTERY_ARTIFACT_DIR"
  {
    printf '#!/usr/bin/env bash\nset -euo pipefail\ncd %q\n' "$PWD"
    printf 'nix develop --command'
    printf ' %q' "$@"
    printf '\n'
  } >"$BATTERY_ARTIFACT_DIR/reproduce.sh"
  chmod +x "$BATTERY_ARTIFACT_DIR/reproduce.sh"
  : >"$BATTERY_NEXTEST_LOG"
}

finalize_battery_artifacts() {
  local status="$1"
  [[ -n "$BATTERY_ARTIFACT_DIR" ]] || return 0
  if [[ "$status" -eq 0 ]]; then
    rm -rf "$BATTERY_ARTIFACT_DIR"
    return 0
  fi

  local daemon_log="${TIDEPOOL_EXTRACT_DAEMON_LOG:-}"
  if [[ -n "$daemon_log" && -f "$daemon_log" ]]; then
    cp "$daemon_log" "$BATTERY_ARTIFACT_DIR/daemon.log"
  fi
  scripts/toolchain-doctor.sh >"$BATTERY_ARTIFACT_DIR/toolchain-doctor.log" 2>&1 || true
  echo "==> test failure artifacts: $BATTERY_ARTIFACT_DIR" >&2
  echo "==> reproduce: $BATTERY_ARTIFACT_DIR/reproduce.sh" >&2
}

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

# Regenerable cache root, mirroring tidepool_toolchain::paths::cache_dir()
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
# servers do (tidepool-toolchain::toolchain, haskell/CLAUDE.md's deployment
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
# Enabled by default. TIDEPOOL_EXTRACT_NO_DAEMON=1 is the unconditional kill
# switch. Memo hits are content-validated by GhcPipeline before reuse.
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

  BATTERY_DAEMON_SOCKET_DIR="$(mktemp -d -t tidepool-extract-daemon.XXXXXX)"
  local sock="$BATTERY_DAEMON_SOCKET_DIR/extract.sock"
  local log="$BATTERY_DAEMON_SOCKET_DIR/daemon.log"
  export TIDEPOOL_EXTRACT_DAEMON_LOG="$log"

  local stamp
  stamp="$(_battery_daemon_stamp_path)"
  local watch_args=()
  if [ -f "$stamp" ]; then
    watch_args=(--watch-stamp "$stamp")
  else
    echo "==> no toolchain stamp at $stamp (nothing deployed via scripts/redeploy.sh on this machine yet) — starting compile daemon without --watch-stamp" >&2
  fi

  echo "==> starting per-run resident compile daemon: socket=$sock log=$log" >&2
  # Rotation and RSS flags are omitted so the frontend owns their defaults.
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
# it's actually reaped (`wait`) — so a caller never proceeds while $1 or its
# own children may still be alive. $2 is a short label for the log lines. Still "exact
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
  unset TIDEPOOL_EXTRACT_DAEMON_LOG
}
