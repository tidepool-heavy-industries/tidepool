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
include_args=(
  --include "$repo_root/haskell/lib"
  --include "$repo_root/haskell/actors"
  --include "$source_dir"
)

if grep -Eq '(^|[^[:alnum:]_.])Tidepool\.Effects([.]Core)?([^[:alnum:]_.]|$)' "$source_file"; then
  cache_root="${XDG_CACHE_HOME:-$HOME/.cache}"
  # The public shim and constructor-bearing Core module are generated into
  # distinct content-addressed roots. Discover the newest copy of each exact
  # module instead of assuming one cache directory contains both.
  for module_path in Tidepool/Effects.hs Tidepool/Effects/Core.hs; do
    newest_module_dir=""
    newest_module_mtime=0
    while IFS= read -r -d '' generated_file; do
      module_dir="${generated_file%/$module_path}"
      module_mtime="$(stat -c %Y "$generated_file" 2>/dev/null || echo 0)"
      if [ "$module_mtime" -gt "$newest_module_mtime" ]; then
        newest_module_dir="$module_dir"
        newest_module_mtime="$module_mtime"
      fi
    done < <(find "$cache_root" -type f -path "*/$module_path" -print0 2>/dev/null)

    if [ -n "$newest_module_dir" ]; then
      include_args+=(--include "$newest_module_dir")
    else
      echo "no generated $module_path found under $cache_root" >&2
    fi
  done
fi

# A unique output directory prevents concurrent workers from sharing GHC files.
output_dir="$(mktemp -d /tmp/tidepool-worker-typecheck.XXXXXX)"
trap 'rm -rf -- "$output_dir"' EXIT

# The extractor worker's compatibility target lookup still derives a module
# name from the target filename. Preserve a normal namespaced module's exact
# identity by staging it under a dotted basename (`Tidepool.Foo.Bar.hs`),
# derived from the repository source root rather than by parsing Haskell.
compiler_source="$source_file"
for module_root in "$repo_root/haskell/lib" "$repo_root/haskell/actors"; do
  case "$source_file" in
    "$module_root"/*.hs)
      relative_module="${source_file#"$module_root"/}"
      module_name="${relative_module%.hs}"
      module_name="${module_name//\//.}"
      compiler_source="$output_dir/$module_name.hs"
      cp -- "$source_file" "$compiler_source"
      break
      ;;
  esac
done

set +e
"$TIDEPOOL_EXTRACT" \
  --all-closed \
  --target-module-only \
  --output-dir "$output_dir" \
  "${include_args[@]}" \
  "$compiler_source" \
  "$@"
extract_status=$?
set -e
exit "$extract_status"
