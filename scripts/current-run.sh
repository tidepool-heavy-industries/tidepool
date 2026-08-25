#!/usr/bin/env bash
# One command for "what's the current selfharness run, and where is its
# state" — replaces the shell archaeology of reconstructing the cache root,
# sorting files by mtime, and correlating a log stream to a lease/run id by
# hand (`tidepool-harness/CLAUDE.md`'s prior documented answer was literally
# `tail -f $(ls -t <cache>/selfharness/log-*.jsonl | head -1)`).
#
# All paths mirror tidepool-runtime::paths::cache_dir() (env precedence:
# XDG_CACHE_HOME -> ~/.cache -> $TMPDIR, joined with "tidepool") and
# tidepool-harness's selfharness::persistence/resume path helpers
# (default_transcript_path, default_checkpoint_path, default_log_path,
# resume::lease_path/segment_path) — read-only, no Rust invoked.
#
# Usage:
#   scripts/current-run.sh [paths]     print every known path + which exist (default)
#   scripts/current-run.sh tail [transcript|log|journal]   tail -f the named stream (default: log)
set -euo pipefail

# cache_dir() lives in lib-extract.sh (shared with scripts/battery.sh's
# compile-daemon stamp-path resolution) rather than duplicated here — root
# CLAUDE.md's "kept-in-sync copies are forbidden" rule. Sourcing it has no
# side effects (it only defines functions); its other helpers are unused here.
source "$(dirname "${BASH_SOURCE[0]}")/lib-extract.sh"

selfharness_dir="$(cache_dir)/selfharness"
lease_file="$selfharness_dir/run-current.json"
checkpoint_file="$selfharness_dir/checkpoint.json"
transcript_file="$selfharness_dir/transcript.jsonl"

newest_matching() {
  # $1: glob pattern (relative to $selfharness_dir)
  [ -d "$selfharness_dir" ] || return 0
  find "$selfharness_dir" -maxdepth 1 -name "$1" -type f -printf '%T@ %p\n' 2>/dev/null \
    | sort -rn | head -1 | cut -d' ' -f2- || true
}

run_id_from_lease() {
  [ -f "$lease_file" ] || return 0
  # Tiny hand-rolled JSON field read (no jq dependency assumed) — the lease
  # is a flat object; RunLease::run_id serializes as "runId" (see
  # tidepool-harness/src/selfharness/resume.rs's #[serde(rename = "runId")]).
  grep -oE '"runId"[[:space:]]*:[[:space:]]*"[^"]*"' "$lease_file" 2>/dev/null \
    | head -1 | sed -E 's/.*:"([^"]*)"/\1/' || true
}

pid_from_lease() {
  [ -f "$lease_file" ] || return 0
  grep -oE '"pid"[[:space:]]*:[[:space:]]*[0-9]+' "$lease_file" 2>/dev/null \
    | head -1 | grep -oE '[0-9]+' || true
}

cmd_paths() {
  echo "== selfharness dir =="
  echo "  $selfharness_dir"
  [ -d "$selfharness_dir" ] || echo "  (does not exist yet — no selfharness run has ever booted here)"
  echo

  echo "== active lease (run-current.json) =="
  if [ -f "$lease_file" ]; then
    run_id="$(run_id_from_lease)"
    pid="$(pid_from_lease)"
    echo "  path:   $lease_file"
    echo "  run_id: ${run_id:-unknown}"
    echo "  pid:    ${pid:-unknown}"
    if [ -n "${pid:-}" ]; then
      if kill -0 "$pid" 2>/dev/null; then
        echo "  status: LIVE — that process still holds this lease"
      else
        echo "  status: dead pid — the next boot will reclaim this lease"
      fi
    fi
  else
    echo "  (none — no run currently active; a retired lease is renamed to run-<id>.json, never deleted)"
    latest_retired="$(newest_matching 'run-*.json')"
    [ -n "$latest_retired" ] && echo "  most recently retired: $latest_retired"
  fi
  echo

  echo "== checkpoint =="
  if [ -f "$checkpoint_file" ]; then
    echo "  $checkpoint_file"
  else
    echo "  $checkpoint_file (absent — first-ever run, no checkpoint written yet)"
  fi
  echo

  echo "== transcript (loop-level driver Event stream) =="
  if [ -f "$transcript_file" ]; then
    echo "  $transcript_file"
  else
    echo "  $transcript_file (absent)"
  fi
  echo

  echo "== newest per-node durable log (log-<epoch>.jsonl) =="
  newest_log="$(newest_matching 'log-*.jsonl')"
  if [ -n "$newest_log" ]; then
    echo "  $newest_log"
  else
    echo "  (none found under $selfharness_dir)"
  fi
  echo

  echo "== journal segments for the active run =="
  run_id="$(run_id_from_lease)"
  if [ -n "${run_id:-}" ]; then
    segs="$(find "$selfharness_dir" -maxdepth 1 -name "journal-${run_id}.*.jsonl" -type f 2>/dev/null | sort || true)"
    if [ -n "$segs" ]; then
      echo "$segs" | sed 's/^/  /'
    else
      echo "  (none found for run_id=$run_id)"
    fi
  else
    echo "  (no active run_id to match against)"
  fi
}

cmd_tail() {
  local which="${1:-log}"
  case "$which" in
    transcript)
      [ -f "$transcript_file" ] || { echo "no transcript at $transcript_file" >&2; exit 1; }
      exec tail -f "$transcript_file"
      ;;
    log)
      local newest_log
      newest_log="$(newest_matching 'log-*.jsonl')"
      [ -n "$newest_log" ] || { echo "no log-*.jsonl under $selfharness_dir" >&2; exit 1; }
      exec tail -f "$newest_log"
      ;;
    journal)
      local run_id newest_seg
      run_id="$(run_id_from_lease)"
      [ -n "${run_id:-}" ] || { echo "no active run_id (no lease at $lease_file)" >&2; exit 1; }
      newest_seg="$(find "$selfharness_dir" -maxdepth 1 -name "journal-${run_id}.*.jsonl" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)"
      [ -n "$newest_seg" ] || { echo "no journal segments for run_id=$run_id" >&2; exit 1; }
      exec tail -f "$newest_seg"
      ;;
    *)
      echo "usage: $0 tail [transcript|log|journal]" >&2
      exit 2
      ;;
  esac
}

case "${1:-paths}" in
  paths) cmd_paths ;;
  tail) shift; cmd_tail "${1:-log}" ;;
  *)
    echo "usage: $0 [paths] | $0 tail [transcript|log|journal]" >&2
    exit 2
    ;;
esac
