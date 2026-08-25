#!/usr/bin/env bash
# Give sandboxed workers the same extractor-backed Haskell check the engine uses,
# without requiring ghc or cabal to be available in the worker shell.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 FILE.hs [-- extra extract args]" >&2
  exit 2
fi

source_file="$1"
shift
if [ "${1:-}" = "--" ]; then
  shift
fi

if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
  echo "TIDEPOOL_EXTRACT is not set" >&2
  exit 1
fi
if [ ! -r "$TIDEPOOL_EXTRACT" ] || [ ! -x "$TIDEPOOL_EXTRACT" ]; then
  echo "TIDEPOOL_EXTRACT is not a readable executable: $TIDEPOOL_EXTRACT" >&2
  exit 1
fi
if [ ! -f "$source_file" ]; then
  echo "Haskell source file not found: $source_file" >&2
  exit 1
fi

# Resolve paths before entering /tmp so relative worker arguments keep working.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$(cd "$(dirname "$source_file")" && pwd)"
source_file="$source_dir/$(basename "$source_file")"
include_args=(--include "$repo_root/haskell/lib" --include "$source_dir")

if grep -Eq '(^|[^[:alnum:]_.])Tidepool\.Effects([^[:alnum:]_.]|$)' "$source_file"; then
  cache_root="${XDG_CACHE_HOME:-$HOME/.cache}"
  newest_effects_dir=""
  newest_effects_mtime=0

  # The engine's cache keys and depth can change; locate the module itself and
  # derive its include root instead of baking either layout into worker prompts.
  shopt -s nullglob
  cache_dirs=("$cache_root"/tidepool*/)
  shopt -u nullglob
  for cache_dir in "${cache_dirs[@]}"; do
    while IFS= read -r -d '' effects_file; do
      effects_dir="${effects_file%/Tidepool/Effects.hs}"
      effects_mtime="$(stat -c %Y "$effects_dir" 2>/dev/null || echo 0)"
      if [ "$effects_mtime" -gt "$newest_effects_mtime" ]; then
        newest_effects_dir="$effects_dir"
        newest_effects_mtime="$effects_mtime"
      fi
    done < <(find "$cache_dir" -type f -path '*/Tidepool/Effects.hs' -print0 2>/dev/null)
  done

  if [ -n "$newest_effects_dir" ]; then
    include_args+=(--include "$newest_effects_dir")
  else
    echo "no generated Tidepool.Effects module found under $cache_root/tidepool*/" >&2
  fi
fi

# A unique output directory prevents concurrent workers from sharing GHC files.
output_dir="$(mktemp -d /tmp/tidepool-worker-typecheck.XXXXXX)"
trap 'rm -rf -- "$output_dir"' EXIT

set +e
"$TIDEPOOL_EXTRACT" \
  --all-closed \
  --target-module-only \
  --output-dir "$output_dir" \
  "${include_args[@]}" \
  "$source_file" \
  "$@"
extract_status=$?
set -e
exit "$extract_status"
