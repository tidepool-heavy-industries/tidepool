#!/usr/bin/env bash
# Shared extractor resolution, validation, and test-daemon lifecycle.
#
# Usage: source this file, then call `resolve_tidepool_extract`. Callers must
# already be cd'd to the repo root since
# the build step below runs `cd bridge/haskell`.
#
# Also owns the resident-compile-daemon lifecycle helpers
# (start_battery_daemon / teardown_battery_daemon) used by test wrappers, and
# the persistent-daemon pair (daemon_start_persistent / daemon_stop_persistent,
# driven by `just daemon-start` / `just daemon-stop`) that keeps one compile
# daemon's GHC module memo warm across separate `just test`/`just check`
# invocations instead of rebooting it every run. Kept here, not duplicated
# per script, for the same reason as resolve_tidepool_extract above.

# A no-argument frontend invocation is a usage error and therefore exits
# non-zero. Capture its complete output without letting `set -e` short-circuit
# the caller, then validate the stable banner used as the frontend identity
# probe.
extract_has_usage_banner() {
  local output
  if output="$("$1" 2>&1)"; then
    :
  fi
  grep -q '^Usage:' <<<"$output"
}

# Print the worktree source inputs compiled into tidepool-extract-bin. The five
# library modules are embedded by Template Haskell in the internal library;
# ordinary bridge/haskell/lib modules are loaded later by the worker and must not make
# the binary permanently appear stale. Keep both freshness callers on this one
# boundary.
tidepool_extract_worker_sources() {
  printf '%s\n' \
    "$PWD/bridge/haskell/src" \
    "$PWD/bridge/haskell/app" \
    "$PWD/bridge/haskell/tidepool-extract.cabal" \
    "$PWD/bridge/haskell/lib/Tidepool/Aeson/Scientific.hs" \
    "$PWD/bridge/haskell/lib/Tidepool/Aeson/Value.hs" \
    "$PWD/bridge/haskell/lib/Tidepool/Command/Types.hs" \
    "$PWD/bridge/haskell/lib/Tidepool/Data/Time.hs" \
    "$PWD/bridge/haskell/lib/Tidepool/Double.hs"
}

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
    ( cd bridge/haskell && cabal build tidepool-extract-bin ) || return 1
    # Cargo owns target-directory, profile, and target-triple resolution.
    # Read its artifact path, including on a fresh=true cache hit.
    TIDEPOOL_EXTRACT="$(
      set -o pipefail
      cargo build -p tidepool-extract-cmd --bin tidepool-extract --message-format=json-render-diagnostics |
        jq -ser '[.[] | select(.reason == "compiler-artifact" and
          .target.name == "tidepool-extract" and .executable != null) |
          .executable] | unique | if length == 1 then .[0] else error("expected one extractor executable") end'
    )" || return 1
    # Split assignment from export: `export VAR="$(cmd)"` masks the command's
    # exit status (SC2155), so a failed list-bin would proceed with an empty
    # var.
    TIDEPOOL_EXTRACT_WORKER="$(cd bridge/haskell && cabal list-bin tidepool-extract-bin)" || return 1
    export TIDEPOOL_EXTRACT TIDEPOOL_EXTRACT_WORKER
  fi

  # Check caller-supplied worktree binaries against the sources that build
  # each half. Nix store timestamps are canonicalized near the epoch, so they
  # cannot support this mtime check.
  if [ "$_was_preset" = 1 ] && [ -x "$TIDEPOOL_EXTRACT" ]; then
    _bin_mtime="$(stat -c %Y "$TIDEPOOL_EXTRACT" 2>/dev/null || echo 0)"
    _plausible_mtime_floor=946684800
    if [ "$_bin_mtime" -gt "$_plausible_mtime_floor" ]; then
      _newest_src="$(find "$PWD/tidepool/extract-cmd/src" "$PWD/tidepool/extract-cmd/Cargo.toml" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      _newest_src="${_newest_src:-0}"
      if [ "$_bin_mtime" -lt "$_newest_src" ]; then
        if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" = "1" ]; then
          echo "warning: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than tidepool-extract-cmd sources — continuing (TIDEPOOL_ALLOW_STALE_EXTRACT=1)" >&2
        else
          echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than tidepool-extract-cmd sources" >&2
          echo "  fix: cargo build -p tidepool-extract-cmd --bin tidepool-extract; cd bridge/haskell && cabal build tidepool-extract-bin" >&2
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
      # `bridge/haskell/lib` is loaded by the worker at evaluation time; it is not a
      # source input to the worker binary. Including it here makes every
      # stdlib-only edit permanently "stale": Cabal correctly declines to
      # rebuild the unaffected executable, so its mtime can never catch up.
      # Keep this boundary identical to scripts/toolchain-doctor.sh.
      mapfile -t _worker_sources < <(tidepool_extract_worker_sources)
      _newest_haskell="$(find "${_worker_sources[@]}" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      if [ "$_worker_mtime" -lt "${_newest_haskell:-0}" ] && [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" != "1" ]; then
        echo "error: TIDEPOOL_EXTRACT_WORKER='$TIDEPOOL_EXTRACT_WORKER' is older than Haskell worker sources" >&2
        exit 1
      fi
    fi
  fi

  if [ ! -x "$TIDEPOOL_EXTRACT" ]; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not executable" >&2
    exit 1
  fi
  if ! extract_has_usage_banner "$TIDEPOOL_EXTRACT"; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
    exit 1
  fi
  echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
}

# The frontend resolves its compiler worker before publishing this identity. EOF
# after the identity is an incomplete request, so the exit status is not a
# successful-request signal. Share this preflight with Exomonad bootstrap.
validate_tidepool_extract_endpoint() (
  local probe_dir endpoint_magic probe_status=0
  probe_dir="$(mktemp -d -t tidepool-endpoint-probe.XXXXXX)" || return 1
  trap 'rm -rf "$probe_dir"' EXIT
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
  timeout --kill-after=5 30 "$TIDEPOOL_EXTRACT" --compiler-endpoint-v1 \
    </dev/null >"$probe_dir/identity" 2>"$probe_dir/stderr" || probe_status=$?
  endpoint_magic="$(od -An -tx1 -N8 "$probe_dir/identity" | tr -d '[:space:]')"
  if [[ "$probe_status" = 124 || "$probe_status" = 137 || "$endpoint_magic" != "5450434944303031" ]] \
    || [[ "$(wc -c <"$probe_dir/identity")" -lt 40 ]]; then
    cat "$probe_dir/stderr" >&2
    echo "error: extractor could not bind a direct compiler endpoint; check TIDEPOOL_EXTRACT and TIDEPOOL_EXTRACT_WORKER" >&2
    return 1
  fi
)

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
BATTERY_DAEMON_START_FAILED=0
BATTERY_ARTIFACT_DIR=""
BATTERY_NEXTEST_LOG=""

# Artifacts for battery entry points. Successful runs leave nothing unless
# TIDEPOOL_KEEP_TEST_LOGS=1 (five runs, 4 MiB per log);
# test or daemon-startup failures retain the exact command, nextest output,
# toolchain report, and compile-daemon log under target/tidepool-test-runs/.
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
  if [[ "$status" -eq 0 && "$BATTERY_DAEMON_START_FAILED" = 0 && "${TIDEPOOL_KEEP_TEST_LOGS:-0}" != 1 ]]; then
    rm -rf "$BATTERY_ARTIFACT_DIR"
    return 0
  fi

  local daemon_log="${TIDEPOOL_EXTRACT_DAEMON_LOG:-}"
  if [[ -n "$daemon_log" && -f "$daemon_log" ]]; then
    cp "$daemon_log" "$BATTERY_ARTIFACT_DIR/daemon.log"
  fi
  local compiler_log="${daemon_log%/*}/compiler.log"
  if [[ -n "$daemon_log" && -f "$compiler_log" ]]; then
    cp "$compiler_log" "$BATTERY_ARTIFACT_DIR/compiler.log"
  fi
  scripts/toolchain-doctor.sh >"$BATTERY_ARTIFACT_DIR/toolchain-doctor.log" 2>&1 || true
  if [[ "$status" -eq 0 && "$BATTERY_DAEMON_START_FAILED" = 0 ]]; then
    # Only marked successful runs are eligible for bounded retention. Never
    # prune failure evidence or arbitrary directories under the artifact root.
    python3 - "$BATTERY_ARTIFACT_DIR" <<'PYLOG'
from pathlib import Path
import shutil
import sys
current = Path(sys.argv[1])
for log in current.glob("*.log"):
    limit = 4 * 1024 * 1024
    if log.stat().st_size > limit:
        with log.open("rb") as stream:
            stream.seek(-limit, 2)
            tail = stream.read()
        log.write_bytes(b"[truncated: last 4 MiB]\n" + tail)
(current / ".successful-run").touch()
runs = sorted((p.parent for p in current.parent.glob("*/.successful-run")),
              key=lambda p: p.stat().st_mtime, reverse=True)
for old in runs[5:]:
    if old != current:
        shutil.rmtree(old)
PYLOG
    echo "==> retained success artifacts (last five runs): $BATTERY_ARTIFACT_DIR" >&2
  else
    echo "==> test/daemon failure artifacts: $BATTERY_ARTIFACT_DIR" >&2
  fi
  echo "==> reproduce: $BATTERY_ARTIFACT_DIR/reproduce.sh" >&2
}

# Best-effort liveness check for an inherited $TIDEPOOL_EXTRACT_DAEMON_SOCKET
# (outer-wrapper respect: a chain script that already started a daemon for
# many shard invocations must not get a second one started underneath it).
# `-S` alone only proves the path is a socket special file, which survives a
# crashed daemon — so also try a real connect via python3 (already used
# by the toolchain scripts) when it's on PATH; without
# python3, fall back to the `-S` check alone. Either way this is advisory: a
# stale socket that refuses connection safely falls back to a direct spawn;
# once connected, ExtractCmd never replays an indeterminate request
# (tidepool/extract-cmd/CLAUDE.md).
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
# servers do (tidepool-toolchain::toolchain, bridge/haskell/CLAUDE.md's deployment
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
# invocation (e.g. battery.sh runs launched back-to-back by another
# script that already started a daemon) must not start, or later tear down,
# a second one. The explicit disable switch still takes precedence.
#
# An unreachable daemon needs no handling here: ExtractCmd::run() (the ONE
# tidepool-extract invocation builder, tidepool/extract-cmd/CLAUDE.md) falls
# back after a known-unsubmitted connect failure. Timeout or crash after
# submission is surfaced rather than replayed. This function only makes
# TIDEPOOL_EXTRACT_DAEMON_SOCKET available; it does not own request policy.
start_battery_daemon() {
  BATTERY_DAEMON_PID=""
  BATTERY_DAEMON_SOCKET_DIR=""
  BATTERY_DAEMON_OWNED=0
  BATTERY_DAEMON_START_FAILED=0

  if [ "${TIDEPOOL_EXTRACT_NO_DAEMON:-0}" = "1" ]; then
    unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
    validate_tidepool_extract_endpoint || return 1
    echo "==> compile daemon disabled; direct compiler endpoint validated" >&2
    return 0
  fi

  if [ -n "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ] && _battery_daemon_socket_alive "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"; then
    echo "==> reusing already-running compile daemon at $TIDEPOOL_EXTRACT_DAEMON_SOCKET (outer wrapper owns its lifecycle)" >&2
    return 0
  fi
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

  # No caller-supplied socket: check for a `just daemon-start`-managed
  # persistent daemon before booting a per-run one. Reused only when it is
  # both alive and current (producer identity matches this invocation's
  # resolved $TIDEPOOL_EXTRACT/$TIDEPOOL_EXTRACT_WORKER); a stale one is left
  # running (never killed out from under whoever started it) and this run
  # falls back to its own per-run daemon below.
  local _persistent_sock _persistent_producer_file
  _persistent_sock="$(_persistent_daemon_dir)/extract.sock"
  _persistent_producer_file="$(_persistent_daemon_dir)/producer"
  if _battery_daemon_socket_alive "$_persistent_sock"; then
    local _current_producer
    _current_producer="$(_current_producer_hex 2>/dev/null)" || _current_producer=""
    if [ -n "$_current_producer" ] && [ -f "$_persistent_producer_file" ] \
      && [ "$(cat "$_persistent_producer_file" 2>/dev/null)" = "$_current_producer" ]; then
      export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$_persistent_sock"
      echo "==> reusing persistent compile daemon at $_persistent_sock" >&2
      return 0
    fi
    echo "==> persistent compile daemon at $_persistent_sock is stale (producer mismatch) — run 'just daemon-stop && just daemon-start' to refresh it; starting a per-run daemon instead" >&2
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

  # The detailed log carries per-request compile costs; with TIDEPOOL_TIMING=1
  # it also carries each phase and every memo miss.
  local compiler_log="$BATTERY_DAEMON_SOCKET_DIR/compiler.log"
  echo "==> starting per-run resident compile daemon: socket=$sock log=$log detail=$compiler_log" >&2
  # Rotation, RSS, and worker-count defaults stay in the frontend; set
  # TIDEPOOL_DAEMON_ARGS (e.g. "--workers 3 --rss-ceiling-mb 7168") to override.
  # Rotation, RSS, and worker-count flags are omitted so the frontend owns
  # their defaults (a persistent daemon defaults to several concurrent GHC
  # workers; see tidepool/extract-cmd/CLAUDE.md).
  "$TIDEPOOL_EXTRACT" --daemon --persistent --socket "$sock" --log-path "$compiler_log" "${watch_args[@]}" ${TIDEPOOL_DAEMON_ARGS:-} >"$log" 2>&1 &
  BATTERY_DAEMON_PID=$!
  BATTERY_DAEMON_OWNED=1
  # Recorded before the boot-wait below so a signal arriving mid-wait still
  # tears this down correctly (the caller installs its cleanup trap before
  # calling this function).

  local started_at=$SECONDS
  while ! _battery_daemon_socket_alive "$sock"; do
    if ! kill -0 "$BATTERY_DAEMON_PID" 2>/dev/null; then
      echo "==> compile daemon exited before readiness (see $log)" >&2
      wait "$BATTERY_DAEMON_PID" 2>/dev/null || true
      BATTERY_DAEMON_PID=""
      _battery_direct_fallback
      return $?
    fi
    if [ $((SECONDS - started_at)) -ge 30 ]; then
      echo "==> compile daemon was not ready within 30s (see $log)" >&2
      _terminate_and_wait "$BATTERY_DAEMON_PID" "compile daemon startup"
      BATTERY_DAEMON_PID=""
      _battery_direct_fallback
      return $?
    fi
    sleep 0.5
  done

  export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$sock"
  BATTERY_DAEMON_OWNED=1
  echo "==> compile daemon up: pid=$BATTERY_DAEMON_PID socket=$sock" >&2
}

_battery_direct_fallback() {
  BATTERY_DAEMON_START_FAILED=1
  BATTERY_DAEMON_OWNED=0
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
  validate_tidepool_extract_endpoint || return 1
  echo "==> compile daemon unavailable; direct compiler endpoint validated, using direct spawn per request" >&2
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
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid" 2>/dev/null || true
    return 0
  fi
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
    if [ "$BATTERY_DAEMON_START_FAILED" = 1 ] && [ -z "$BATTERY_ARTIFACT_DIR" ]; then
      echo "==> retained compile daemon startup log: $BATTERY_DAEMON_SOCKET_DIR/daemon.log" >&2
    else
      rm -rf "$BATTERY_DAEMON_SOCKET_DIR"
    fi
  fi
  if [ "$BATTERY_DAEMON_OWNED" = 1 ]; then
    unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
  fi
  BATTERY_DAEMON_SOCKET_DIR=""
  BATTERY_DAEMON_OWNED=0
  unset TIDEPOOL_EXTRACT_DAEMON_LOG
}

# --- Persistent compile daemon ---
#
# A second daemon lifecycle, independent of start_battery_daemon/
# teardown_battery_daemon above: those boot a fresh daemon per script
# invocation and always tear it down at exit, so the GHC module memo dies
# with every `just test`. daemon_start_persistent instead boots one
# long-lived daemon under a well-known directory and leaves it running past
# the invoking shell; start_battery_daemon's reuse check picks it up
# automatically (see above) whenever the caller has not already supplied its
# own $TIDEPOOL_EXTRACT_DAEMON_SOCKET. `just daemon-start` / `just
# daemon-stop` are the direct entry points.
#
# Directory layout under <cache_dir>/battery-daemon/ (cache_dir() above):
#   extract.sock  - the daemon's listening socket
#   daemon.pid    - pid of the daemon process this helper started
#   daemon.log    - the daemon's stdout+stderr (includes its "compiler
#                   daemon ready ... producer=<hex>" banner)
#   compiler.log  - the daemon's --log-path detailed/trace log
#   producer      - hex producer identity recorded at the daemon's last
#                   successful start, used to detect staleness below

_persistent_daemon_dir() {
  echo "$(cache_dir)/battery-daemon"
}

# Producer identity of the CURRENTLY resolved $TIDEPOOL_EXTRACT +
# $TIDEPOOL_EXTRACT_WORKER, read without booting a GHC worker session.
# tidepool-extract has no standalone "print producer identity" flag; the
# cheapest existing path is --compiler-endpoint-v1
# (tidepool/extract-cmd/src/frontend.rs serve_bound_endpoint), which writes
# an 8-byte magic plus the 32-byte PreparedWorker::producer_identity() hash
# to stdout and then blocks reading a transaction prefix from stdin. Piping
# /dev/null in makes that read hit EOF immediately, so the process exits
# right after writing the identity bytes and never spawns a GHC worker
# process. This is the same probe validate_tidepool_extract_endpoint above
# uses to prove a bindable endpoint (same magic/timeout checks, reused here)
# — just reading the identity's producer half instead of only its presence.
# It is exactly the value the daemon itself later logs as `producer=<hex>`
# in "compiler daemon ready" (tidepool/extract-cmd/src/daemon.rs), since
# both come from the same PreparedWorker::producer_identity() call.
_current_producer_hex() {
  local probe status=0
  probe="$(mktemp -t tidepool-producer-probe.XXXXXX)" || return 1
  timeout --kill-after=5 30 "$TIDEPOOL_EXTRACT" --compiler-endpoint-v1 \
    </dev/null >"$probe" 2>/dev/null || status=$?
  local magic
  magic="$(od -An -tx1 -N8 "$probe" | tr -d '[:space:]')"
  if [[ "$status" = 124 || "$status" = 137 || "$magic" != "5450434944303031" ]] \
    || [[ "$(wc -c <"$probe")" -lt 40 ]]; then
    rm -f "$probe"
    return 1
  fi
  # -v disables od's default elision of repeated identical output lines
  # (`*`): the 32-byte producer hash spans multiple 16-byte lines and a
  # producer with a repeated byte pattern would otherwise come back
  # truncated.
  od -v -An -tx1 -j8 -N32 "$probe" | tr -d '[:space:]'
  rm -f "$probe"
}

# Starts (or reuses) the persistent compile daemon and prints
# `export TIDEPOOL_EXTRACT_DAEMON_SOCKET=<sock>`. Idempotent: a live daemon
# whose recorded producer identity still matches the current one is reused
# as-is and left running. A stale (producer mismatch) or dead/half-started
# daemon left in the directory is cleaned up (recorded pid terminated if
# still alive) before a fresh one is launched. Must run after
# resolve_tidepool_extract (needs $TIDEPOOL_EXTRACT).
daemon_start_persistent() {
  local dir sock pidfile log compiler_log producer_file
  dir="$(_persistent_daemon_dir)"
  mkdir -p "$dir"
  sock="$dir/extract.sock"
  pidfile="$dir/daemon.pid"
  log="$dir/daemon.log"
  compiler_log="$dir/compiler.log"
  producer_file="$dir/producer"

  local current_producer
  current_producer="$(_current_producer_hex)" || {
    echo "error: could not probe the current compiler producer identity via '$TIDEPOOL_EXTRACT --compiler-endpoint-v1'" >&2
    return 1
  }

  if _battery_daemon_socket_alive "$sock"; then
    if [ -f "$producer_file" ] && [ "$(cat "$producer_file" 2>/dev/null)" = "$current_producer" ]; then
      export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$sock"
      echo "==> persistent compile daemon already running: socket=$sock" >&2
      echo "export TIDEPOOL_EXTRACT_DAEMON_SOCKET=$sock"
      return 0
    fi
    echo "==> persistent compile daemon at $sock is stale (producer changed) — restarting" >&2
    daemon_stop_persistent
  elif [ -f "$pidfile" ] || [ -e "$sock" ]; then
    echo "==> clearing stale persistent compile daemon state in $dir" >&2
    daemon_stop_persistent
  fi

  local stamp watch_args=()
  stamp="$(_battery_daemon_stamp_path)"
  if [ -f "$stamp" ]; then
    watch_args=(--watch-stamp "$stamp")
  fi

  echo "==> starting persistent compile daemon: socket=$sock log=$log detail=$compiler_log" >&2
  # The extractor arms PDEATHSIG against its launcher (tidepool/extract-cmd
  # process.rs), so it cannot be detached directly: it would die with the
  # `just daemon-start` shell. A setsid'd bash keeper stays as its parent
  # and waits on it; the pid file records the daemon itself. Rotation, RSS,
  # and worker-count flags are omitted so the frontend owns their defaults,
  # matching start_battery_daemon above.
  rm -f "$pidfile"
  # The daemon outlives this dev shell, whose TMPDIR is removed when it exits;
  # give it a TMPDIR of its own beside its socket.
  mkdir -p "$dir/tmp"
  TMPDIR="$dir/tmp" TMP="$dir/tmp" TEMP="$dir/tmp" TEMPDIR="$dir/tmp" PERSISTENT_PIDFILE="$pidfile" setsid bash -c '"$@" </dev/null & echo "$!" >"$PERSISTENT_PIDFILE"; wait "$!"' \
    persistent-daemon-keeper \
    "$TIDEPOOL_EXTRACT" --daemon --persistent --socket "$sock" --log-path "$compiler_log" "${watch_args[@]}" ${TIDEPOOL_DAEMON_ARGS:-} \
    </dev/null >"$log" 2>&1 &
  local pid=""
  local pid_wait=0
  while [ -z "$pid" ] && [ "$pid_wait" -lt 50 ]; do
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    [ -n "$pid" ] || { sleep 0.1; pid_wait=$((pid_wait + 1)); }
  done
  if [ -z "$pid" ]; then
    echo "error: persistent compile daemon keeper did not report a pid (see $log)" >&2
    return 1
  fi

  local started_at=$SECONDS
  while ! _battery_daemon_socket_alive "$sock"; do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "error: persistent compile daemon exited before readiness (see $log)" >&2
      wait "$pid" 2>/dev/null || true
      rm -f "$pidfile"
      return 1
    fi
    if [ $((SECONDS - started_at)) -ge 30 ]; then
      echo "error: persistent compile daemon was not ready within 30s (see $log)" >&2
      _terminate_and_wait "$pid" "persistent compile daemon startup"
      rm -f "$pidfile"
      return 1
    fi
    sleep 0.5
  done

  printf '%s\n' "$current_producer" >"$producer_file"
  # Recorded so a later, unrelated `just daemon-stop` shell (which never
  # calls resolve_tidepool_extract itself) can still ask THIS daemon to stop
  # gracefully with its own binary, matching producer_file's convention.
  printf '%s\n' "$TIDEPOOL_EXTRACT" >"$dir/daemon.exe"
  export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$sock"
  echo "==> persistent compile daemon up: pid=$pid socket=$sock" >&2
  echo "export TIDEPOOL_EXTRACT_DAEMON_SOCKET=$sock"
}

# Terminates the recorded persistent daemon (if any) and removes its
# socket/pid/producer files. Succeeds quietly if nothing is running.
#
# Prefers a graceful stop: the daemon's own `--stop-daemon` frontend mode
# (tidepool/extract-cmd/src/frontend.rs) sends the wire's STOP kind
# (tidepool/extract-cmd/src/daemon.rs), which lets any in-flight compile
# finish before the daemon retires its socket and exits on its own. Signaling
# it directly (_terminate_and_wait, below) has no such orderly path — the
# daemon does not handle SIGTERM — and can tear down a compile another
# concurrent caller is waiting on. The running daemon's own binary path,
# recorded by daemon_start_persistent at launch (daemon.exe, alongside
# producer_file), is invoked rather than a resolved $TIDEPOOL_EXTRACT: this
# function runs from `just daemon-stop` before any resolve_tidepool_extract,
# and it must speak the exact wire the running daemon does.
daemon_stop_persistent() {
  local dir sock pidfile producer_file exe_file
  dir="$(_persistent_daemon_dir)"
  sock="$dir/extract.sock"
  pidfile="$dir/daemon.pid"
  producer_file="$dir/producer"
  exe_file="$dir/daemon.exe"

  if [ -f "$pidfile" ]; then
    local pid
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      local stop_bin=""
      [ -f "$exe_file" ] && stop_bin="$(cat "$exe_file" 2>/dev/null || true)"
      if [ -n "$stop_bin" ] && [ -x "$stop_bin" ]; then
        echo "==> requesting graceful stop of persistent compile daemon (pid $pid); waiting for in-flight compile to finish" >&2
        if timeout --kill-after=5 30 "$stop_bin" --stop-daemon --socket "$sock" >/dev/null 2>&1; then
          local wait_started=$SECONDS
          while kill -0 "$pid" 2>/dev/null; do
            if [ $((SECONDS - wait_started)) -ge 120 ]; then
              echo "==> persistent compile daemon (pid $pid) did not exit within 120s of a graceful stop request — falling back to termination" >&2
              break
            fi
            sleep 0.5
          done
        else
          echo "==> graceful stop request to persistent compile daemon (pid $pid) failed — falling back to termination" >&2
        fi
      fi
      if kill -0 "$pid" 2>/dev/null; then
        _terminate_and_wait "$pid" "persistent compile daemon"
        echo "==> persistent compile daemon (pid $pid) stopped" >&2
      else
        wait "$pid" 2>/dev/null || true
        echo "==> persistent compile daemon (pid $pid) stopped gracefully" >&2
      fi
    fi
  fi
  rm -f "$sock" "$pidfile" "$producer_file" "$exe_file"
}
