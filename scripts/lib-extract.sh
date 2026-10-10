#!/usr/bin/env bash
# Verified native bundle selection and compile-daemon process custody.
# Production host execution uses the frozen qualification owner directly.

select_native_bundle() {
  local descriptor="${1:?qualification descriptor required}" selection
  [[ $# -eq 1 ]] || { echo 'error: select_native_bundle accepts only a qualification descriptor' >&2; return 2; }
  local owner="$(dirname -- "$descriptor")/qualification.py"
  [[ -f "$owner" ]] || { echo "error: frozen bundle qualification owner missing: $owner" >&2; return 1; }
  NATIVE_OPERATOR_PYTHON="$(command -v python3)" || return 1
  selection="$("$NATIVE_OPERATOR_PYTHON" "$owner" environment "$descriptor" --shell)" || return 1
  eval "$selection"
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
  if [[ "$probe_status" = 124 || "$probe_status" = 137 || "$endpoint_magic" != "5450434944303032" ]] \
    || [[ "$(wc -c <"$probe_dir/identity")" -lt 72 ]]; then
    cat "$probe_dir/stderr" >&2
    echo "error: extractor could not bind a direct compiler endpoint; check TIDEPOOL_EXTRACT and TIDEPOOL_EXTRACT_WORKER" >&2
    return 1
  fi
)

# --- Resident compile daemon ---
#
# Globals set by start_compile_daemon and read by teardown_compile_daemon:
#   COMPILE_DAEMON_PID         pid of the daemon THIS process started, or ""
#   COMPILE_DAEMON_SOCKET_DIR  per-run tempdir holding the socket+log, or ""
#   COMPILE_DAEMON_OWNED       1 iff this process owns the daemon's lifecycle
#                               (0 when reusing an outer wrapper's daemon, or
#                               when the daemon is disabled/failed to start)
COMPILE_DAEMON_PID=""
COMPILE_DAEMON_SOCKET_DIR=""
COMPILE_DAEMON_OWNED=0
COMPILE_DAEMON_START_FAILED=0
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
_compile_daemon_socket_alive() {
  local sock="$1"
  [ -S "$sock" ] || return 1
  if command -v "${NATIVE_OPERATOR_PYTHON:-python3}" >/dev/null 2>&1; then
    "${NATIVE_OPERATOR_PYTHON:-python3}" - "$sock" >/dev/null 2>&1 <<'PY'
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
# shell consumer of that precedence.
cache_dir() {
  if [ -n "${XDG_CACHE_HOME:-}" ]; then
    echo "${XDG_CACHE_HOME}/tidepool"
  elif [ -n "${HOME:-}" ]; then
    echo "${HOME}/.cache/tidepool"
  else
    echo "${TMPDIR:-/tmp}/tidepool"
  fi
}

# Starts a per-run resident compile daemon and exports
# TIDEPOOL_EXTRACT_DAEMON_SOCKET for the caller's native test invocation.
# Must run after select_native_bundle (needs $TIDEPOOL_EXTRACT).
#
# Enabled by default. TIDEPOOL_EXTRACT_NO_DAEMON=1 is the unconditional kill
# switch. Memo hits are content-validated by GhcPipeline before reuse.
#
# Outer-wrapper respect: if $TIDEPOOL_EXTRACT_DAEMON_SOCKET is already set
# and looks alive, reuse it and leave COMPILE_DAEMON_OWNED=0 — a chain
# invocation (e.g. native tests launched back-to-back by another
# script that already started a daemon) must not start, or later tear down,
# a second one. The explicit disable switch still takes precedence.
#
# An unreachable daemon needs no handling here: ExtractCmd::run() (the ONE
# tidepool-extract invocation builder, tidepool/extract-cmd/CLAUDE.md) falls
# back after a known-unsubmitted connect failure. Timeout or crash after
# submission is surfaced rather than replayed. This function only makes
# TIDEPOOL_EXTRACT_DAEMON_SOCKET available; it does not own request policy.
start_compile_daemon() {
  COMPILE_DAEMON_PID=""
  COMPILE_DAEMON_SOCKET_DIR=""
  COMPILE_DAEMON_OWNED=0
  COMPILE_DAEMON_START_FAILED=0

  local measurement=0
  [ "${TIDEPOOL_EXTRACT_MEASUREMENT:-0}" != 1 ] || measurement=1
  if [ "$measurement" = 1 ]; then
    if [ -n "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ]; then
      echo "error: measurement mode requires an exclusively owned compile daemon; inherited TIDEPOOL_EXTRACT_DAEMON_SOCKET is not allowed" >&2
      return 1
    fi
    if [ "${TIDEPOOL_EXTRACT_NO_DAEMON:-0}" = 1 ]; then
      echo "error: measurement mode requires an owned compile daemon; TIDEPOOL_EXTRACT_NO_DAEMON=1 is not allowed" >&2
      return 1
    fi
    unset TIDEPOOL_PERFORMANCE_COMPILER_TRACE \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH
  fi

  if [ "${TIDEPOOL_EXTRACT_NO_DAEMON:-0}" = "1" ]; then
    unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
    validate_tidepool_extract_endpoint || return 1
    echo "==> compile daemon disabled; direct compiler endpoint validated" >&2
    return 0
  fi

  if [ "$measurement" != 1 ] && [ -n "${TIDEPOOL_EXTRACT_DAEMON_SOCKET:-}" ] && _compile_daemon_socket_alive "$TIDEPOOL_EXTRACT_DAEMON_SOCKET"; then
    echo "==> reusing already-running compile daemon at $TIDEPOOL_EXTRACT_DAEMON_SOCKET (outer wrapper owns its lifecycle)" >&2
    return 0
  fi
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

  # A current persistent daemon may be reused without taking its lifecycle.
  # A stale one remains owned by its original launcher; this caller starts a
  # separate daemon capped at one worker instead.
  local _persistent_sock _persistent_producer_file _per_run_args=()
  _persistent_sock="$(_persistent_daemon_dir)/extract.sock"
  _persistent_producer_file="$(_persistent_daemon_dir)/producer"
  if [ "$measurement" != 1 ] && _compile_daemon_socket_alive "$_persistent_sock"; then
    local _current_producer
    _current_producer="$(_current_producer_hex 2>/dev/null)" || _current_producer=""
    if [ -n "$_current_producer" ] && [ -f "$_persistent_producer_file" ] \
      && [ "$(cat "$_persistent_producer_file" 2>/dev/null)" = "$_current_producer" ]; then
      export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$_persistent_sock"
      echo "==> reusing persistent compile daemon at $_persistent_sock" >&2
      return 0
    fi
    local _why="producer mismatch"
    case " ${TIDEPOOL_DAEMON_ARGS:-} " in
      *" --workers "*) ;;
      *) _per_run_args=(--workers 1) ;;
    esac
    echo "==> persistent compile daemon at $_persistent_sock is stale ($_why) — run 'just daemon-stop && just daemon-start' to refresh it; starting a per-run daemon${_per_run_args[*]:+ with one GHC worker} instead" >&2
  fi

  COMPILE_DAEMON_SOCKET_DIR="$(mktemp -d -t tidepool-extract-daemon.XXXXXX)"
  local sock="$COMPILE_DAEMON_SOCKET_DIR/extract.sock"
  local log="$COMPILE_DAEMON_SOCKET_DIR/daemon.log"
  export TIDEPOOL_EXTRACT_DAEMON_LOG="$log"

  # The detailed log carries per-request compile costs; with TIDEPOOL_TIMING=1
  # it also carries each phase and every memo miss.
  local compiler_log="$COMPILE_DAEMON_SOCKET_DIR/compiler.log"
  echo "==> starting per-run resident compile daemon: socket=$sock log=$log detail=$compiler_log" >&2
  # Rotation, RSS, and worker-count defaults stay in the frontend; set
  # TIDEPOOL_DAEMON_ARGS (e.g. "--workers 3 --rss-ceiling-mb 7168") to override.
  # Rotation, RSS, and worker-count flags are omitted so the frontend owns
  # their defaults (a persistent daemon defaults to several concurrent GHC
  # workers; see tidepool/extract-cmd/CLAUDE.md).
  "$TIDEPOOL_EXTRACT" --daemon --persistent --socket "$sock" --log-path "$compiler_log" "${_per_run_args[@]}" ${TIDEPOOL_DAEMON_ARGS:-} >"$log" 2>&1 &
  COMPILE_DAEMON_PID=$!
  COMPILE_DAEMON_OWNED=1
  # Recorded before the boot-wait below so a signal arriving mid-wait still
  # tears this down correctly (the caller installs its cleanup trap before
  # calling this function).

  local started_at=$SECONDS
  while ! _compile_daemon_socket_alive "$sock"; do
    if ! kill -0 "$COMPILE_DAEMON_PID" 2>/dev/null; then
      echo "==> compile daemon exited before readiness (see $log)" >&2
      wait "$COMPILE_DAEMON_PID" 2>/dev/null || true
      COMPILE_DAEMON_PID=""
      if [ "$measurement" = 1 ]; then
        COMPILE_DAEMON_START_FAILED=1
        echo "error: measurement compile daemon exited before readiness; direct fallback is forbidden (see $log)" >&2
        return 1
      fi
      _compile_direct_fallback
      return $?
    fi
    if [ $((SECONDS - started_at)) -ge 30 ]; then
      echo "==> compile daemon was not ready within 30s (see $log)" >&2
      _terminate_and_wait "$COMPILE_DAEMON_PID" "compile daemon startup"
      COMPILE_DAEMON_PID=""
      if [ "$measurement" = 1 ]; then
        COMPILE_DAEMON_START_FAILED=1
        echo "error: measurement compile daemon was not ready within 30s; direct fallback is forbidden (see $log)" >&2
        return 1
      fi
      _compile_direct_fallback
      return $?
    fi
    sleep 0.5
  done

  export TIDEPOOL_EXTRACT_DAEMON_SOCKET="$sock"
  COMPILE_DAEMON_OWNED=1
  echo "==> compile daemon up: pid=$COMPILE_DAEMON_PID socket=$sock" >&2
  if [ "$measurement" = 1 ]; then
    local identity expected_producer deadline=$((SECONDS + 10))
    expected_producer="$(_current_producer_hex 2>/dev/null)" || {
      echo "error: could not determine the resolved compile producer identity" >&2
      COMPILE_DAEMON_START_FAILED=1
      teardown_compile_daemon --preserve-logs
      return 1
    }
    while :; do
      identity="$(_measurement_daemon_identity "${compiler_log%.log}.jsonl" "$COMPILE_DAEMON_PID" "$expected_producer")" || identity=""
      [ -n "$identity" ] && break
      if [ "$SECONDS" -ge "$deadline" ] || ! kill -0 "$COMPILE_DAEMON_PID" 2>/dev/null; then
        echo "error: owned compile daemon did not publish matching producer/pid/epoch evidence (see $log and $compiler_log)" >&2
        COMPILE_DAEMON_START_FAILED=1
        teardown_compile_daemon --preserve-logs
        return 1
      fi
      sleep 0.1
    done
    IFS=$'\t' read -r TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH <<<"$identity"
    TIDEPOOL_PERFORMANCE_COMPILER_TRACE="${compiler_log%.log}.jsonl"
    export TIDEPOOL_PERFORMANCE_COMPILER_TRACE \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER \
      TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH
    echo "==> measurement daemon identity: producer=$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER pid=$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID epoch=$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH trace=$TIDEPOOL_PERFORMANCE_COMPILER_TRACE" >&2
  fi
}

# Emits the ready record's producer, daemon PID and epoch only when it matches
# this owned process and the resolved frontend's producer identity.
_measurement_daemon_identity() {
  "${NATIVE_OPERATOR_PYTHON:-python3}" - "$1" "$2" "$3" <<'PYIDENTITY'
import json, re, sys
path, expected_pid, expected_producer = sys.argv[1:]
if not expected_producer:
    raise SystemExit(1)
try:
    lines = open(path, encoding="utf-8")
except OSError:
    raise SystemExit(1)
match = None
ready_records = 0
for line in lines:
    try:
        event = json.loads(line)
    except (json.JSONDecodeError, UnicodeDecodeError):
        continue
    fields = event.get("fields", {})
    if fields.get("message") != "compiler daemon ready":
        continue
    ready_records += 1
    producer = fields.get("producer", "")
    pid = str(fields.get("daemon_pid", ""))
    epoch = fields.get("daemon_epoch", "")
    if (producer == expected_producer and pid == expected_pid
            and re.fullmatch(r"[0-9a-f]{64}", epoch)):
        match = (producer, pid, epoch)
if ready_records == 1 and match:
    print("\t".join(match))
else:
    raise SystemExit(1)
PYIDENTITY
}

_compile_direct_fallback() {
  COMPILE_DAEMON_START_FAILED=1
  COMPILE_DAEMON_OWNED=0
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
  validate_tidepool_extract_endpoint || return 1
  echo "==> compile daemon unavailable; direct compiler endpoint validated, using direct spawn per request" >&2
}

# Terminate only the recorded daemon PID, escalate after ten seconds, and reap
# children owned by this shell. Shared by transient and persistent daemon stops.
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

# Tears down a daemon this process started (no-op if COMPILE_DAEMON_OWNED=0
# — disabled, failed to start, or reusing an outer wrapper's daemon), via
# _terminate_and_wait above. Call from an EXIT trap installed BEFORE
# start_compile_daemon runs, so it also fires if a signal lands mid-boot
# (see start_compile_daemon's comment).
teardown_compile_daemon() {
  local preserve_logs=0
  [ "${1:-}" != "--preserve-logs" ] || preserve_logs=1
  if [ "$COMPILE_DAEMON_OWNED" = 1 ] && [ -n "$COMPILE_DAEMON_PID" ]; then
    _terminate_and_wait "$COMPILE_DAEMON_PID" "compile daemon"
    echo "==> compile daemon (pid $COMPILE_DAEMON_PID) torn down" >&2
  fi
  COMPILE_DAEMON_PID=""
  if [ "$preserve_logs" = 0 ] && [ -n "$COMPILE_DAEMON_SOCKET_DIR" ] && [ -d "$COMPILE_DAEMON_SOCKET_DIR" ]; then
    if [ "$COMPILE_DAEMON_START_FAILED" = 1 ]; then
      echo "==> retained compile daemon startup log: $COMPILE_DAEMON_SOCKET_DIR/daemon.log" >&2
    else
      rm -rf "$COMPILE_DAEMON_SOCKET_DIR"
    fi
  fi
  if [ "$COMPILE_DAEMON_OWNED" = 1 ]; then
    unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
  fi
  if [ "$preserve_logs" = 0 ]; then
    COMPILE_DAEMON_SOCKET_DIR=""
    COMPILE_DAEMON_OWNED=0
    unset TIDEPOOL_EXTRACT_DAEMON_LOG
  fi
}

# Persistent daemon operations retain their exact endpoint and producer.
# Stopping uses that recorded frontend, not a newly selected bundle.

# Mirrors tidepool-toolchain's paths::persistent_compile_daemon_socket, which
# `exomonad init`'s preflight reads; change both together.
_persistent_daemon_dir() {
  echo "$(cache_dir)/battery-daemon"
}

# Producer identity of the CURRENTLY resolved $TIDEPOOL_EXTRACT +
# $TIDEPOOL_EXTRACT_WORKER, read without booting a GHC worker session.
# tidepool-extract has no standalone "print producer identity" flag; the
# cheapest existing path is --compiler-endpoint-v1
# (tidepool/extract-cmd/src/frontend.rs serve_bound_endpoint), which writes
# an 8-byte magic, the 32-byte PreparedWorker::producer_identity() hash, and
# the 32-byte selected-worker identity to stdout before reading a transaction
# prefix from stdin. Piping
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
  if [[ "$status" = 124 || "$status" = 137 || "$magic" != "5450434944303032" ]] \
    || [[ "$(wc -c <"$probe")" -lt 72 ]]; then
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
# select_native_bundle (needs $TIDEPOOL_EXTRACT).
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

  if _compile_daemon_socket_alive "$sock"; then
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

  echo "==> starting persistent compile daemon: socket=$sock log=$log detail=$compiler_log" >&2
  # The extractor arms PDEATHSIG against its launcher (tidepool/extract-cmd
  # process.rs), so it cannot be detached directly: it would die with the
  # `just daemon-start` shell. A setsid'd bash keeper stays as its parent
  # and waits on it; the pid file records the daemon itself. Rotation, RSS,
  # and worker-count flags are omitted so the frontend owns their defaults,
  # matching start_compile_daemon above.
  rm -f "$pidfile"
  # The daemon outlives its launcher and needs a persistent temporary directory;
  # give it a TMPDIR of its own beside its socket.
  mkdir -p "$dir/tmp"
  TMPDIR="$dir/tmp" TMP="$dir/tmp" TEMP="$dir/tmp" TEMPDIR="$dir/tmp" PERSISTENT_PIDFILE="$pidfile" setsid bash -c '"$@" </dev/null & echo "$!" >"$PERSISTENT_PIDFILE"; wait "$!"' \
    persistent-daemon-keeper \
    "$TIDEPOOL_EXTRACT" --daemon --persistent --socket "$sock" --log-path "$compiler_log" ${TIDEPOOL_DAEMON_ARGS:-} \
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
  while ! _compile_daemon_socket_alive "$sock"; do
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
  # selects a bundle itself) can still ask THIS daemon to stop
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
# function runs from `just daemon-stop` before any bundle selection,
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
  rm -f "$sock" "$pidfile" "$producer_file" "$exe_file" "$dir/daemon.worker" "$dir/sources"
}
