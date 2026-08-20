#!/usr/bin/env bash
# Archive one selfharness run's durable state so a fresh boot starts clean.
#
#   scripts/companion-archive.sh <state-dir> <archive-name>
#
# MOVES (renames, same filesystem — never copies) checkpoint.json,
# run-current.json, transcript.jsonl, journal-*.jsonl, and log-*.jsonl out of
# <state-dir> into <state-dir>/<archive-name>, leaving <state-dir> ready for
# a fresh boot. Refuses if the archive dir already exists (no clobbering a
# prior archive) or if <state-dir>/run-current.json names a still-alive pid
# (the one lease/lock file this tree writes — see
# tidepool-harness/src/selfharness/resume.rs's module doc, `{"runId": ...,
# "pid": ..., "startedAt": ...}`). If no run-current.json is present, this
# script has no lock to check: the operator must stop the process themselves
# before archiving.
set -euo pipefail

usage() {
  echo "usage: $0 <state-dir> <archive-name>" >&2
  exit 1
}

[ "$#" -eq 2 ] || usage
state_dir=$1
archive_name=$2

[ -d "$state_dir" ] || {
  echo "companion-archive: state dir not found: $state_dir" >&2
  exit 1
}

archive_dir="$state_dir/$archive_name"
if [ -e "$archive_dir" ]; then
  echo "companion-archive: archive already exists, refusing to overwrite: $archive_dir" >&2
  exit 1
fi

lease_file="$state_dir/run-current.json"
if [ -f "$lease_file" ]; then
  pid=$(grep -o '"pid"[[:space:]]*:[[:space:]]*[0-9][0-9]*' "$lease_file" | grep -o '[0-9][0-9]*$' || true)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "companion-archive: refusing — $lease_file names pid $pid, which is still alive. Stop the selfharness process first." >&2
    exit 1
  fi
fi

to_move=()
for f in "$state_dir/checkpoint.json" "$state_dir/run-current.json" "$state_dir/transcript.jsonl"; do
  if [ -e "$f" ]; then
    to_move+=("$f")
  fi
done
shopt -s nullglob
for f in "$state_dir"/journal-*.jsonl "$state_dir"/log-*.jsonl; do
  to_move+=("$f")
done
shopt -u nullglob

if [ "${#to_move[@]}" -eq 0 ]; then
  echo "companion-archive: nothing to archive in $state_dir" >&2
  exit 1
fi

mkdir -p "$archive_dir"
for f in "${to_move[@]}"; do
  mv -- "$f" "$archive_dir/"
done

echo "companion-archive: moved ${#to_move[@]} file(s) from $state_dir into $archive_dir"
