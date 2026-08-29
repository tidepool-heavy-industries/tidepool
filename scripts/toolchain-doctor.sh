#!/usr/bin/env bash
# Report the public extractor frontend and, for worktree builds, the compiler
# worker it will launch. This is diagnostic only; it never builds or mutates.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo_root="$PWD"
status=0
plausible_mtime_floor=946684800

note() { echo "  $*"; }
fail() { echo "  ERROR: $*" >&2; status=1; }

freshness() {
  local label="$1" binary="$2"
  shift 2
  local binary_mtime newest_mtime
  binary_mtime="$(stat -c %Y "$binary" 2>/dev/null || echo 0)"
  newest_mtime="$(find "$@" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
  newest_mtime="${newest_mtime:-0}"
  if [ "$binary_mtime" -le "$plausible_mtime_floor" ]; then
    note "$label freshness: not applicable (Nix store timestamp)"
  elif [ "$binary_mtime" -lt "$newest_mtime" ]; then
    fail "$label is older than its worktree sources"
  else
    note "$label freshness: current"
  fi
}

echo "== extractor frontend =="
frontend="${TIDEPOOL_EXTRACT:-}"
frontend_source="\$TIDEPOOL_EXTRACT"
if [ -z "$frontend" ] && [ -x "$repo_root/target/debug/tidepool-extract" ]; then
  frontend="$repo_root/target/debug/tidepool-extract"
  frontend_source="worktree target/debug"
elif [ -z "$frontend" ] && command -v tidepool-extract >/dev/null 2>&1; then
  frontend="$(command -v tidepool-extract)"
  frontend_source="PATH"
fi

if [ -z "$frontend" ]; then
  fail "no frontend found (set TIDEPOOL_EXTRACT or build tidepool-extract-cmd)"
elif [ ! -x "$frontend" ]; then
  fail "frontend is not executable: $frontend"
else
  note "path:   $frontend"
  note "source: $frontend_source"
  usage_output="$("$frontend" 2>&1)"
  if grep -q '^Usage:' <<<"$usage_output"; then
    note "usage probe: ok"
  else
    fail "frontend did not print its Usage banner"
  fi
  if [ "$frontend_source" = "worktree target/debug" ]; then
    freshness "frontend" "$frontend" "$repo_root/tidepool-extract-cmd/src" "$repo_root/tidepool-extract-cmd/Cargo.toml"
  fi
fi

echo
echo "== compiler worker =="
worker="${TIDEPOOL_EXTRACT_WORKER:-}"
worker_source="\$TIDEPOOL_EXTRACT_WORKER"
if [ -z "$worker" ] && worker="$(cd haskell && cabal list-bin tidepool-extract-bin 2>/dev/null)" && [ -x "$worker" ]; then
  worker_source="cabal worktree build"
fi

if [ "$frontend_source" = "PATH" ] && [ -z "${TIDEPOOL_EXTRACT_WORKER:-}" ]; then
  note "provided by installed frontend wrapper"
elif [ -z "$worker" ]; then
  fail "no worker found (set TIDEPOOL_EXTRACT_WORKER or run: cd haskell && cabal build tidepool-extract-bin)"
elif [ ! -x "$worker" ]; then
  fail "worker is not executable: $worker"
else
  note "path:   $worker"
  note "source: $worker_source"
  freshness "worker" "$worker" "$repo_root/haskell/src" "$repo_root/haskell/app" "$repo_root/haskell/tidepool-extract.cabal"
fi

echo
echo "== GHC runtime =="
if [ "$frontend_source" = "PATH" ] && [ -z "${TIDEPOOL_EXTRACT_WORKER:-}" ]; then
  note "provided by installed frontend wrapper"
elif ghc_path="$(command -v ghc 2>/dev/null)"; then
  note "path:    $ghc_path"
  note "version: $(ghc --version 2>/dev/null || echo unknown)"
  if command -v ghc-pkg >/dev/null 2>&1 && ghc-pkg list 2>/dev/null | grep -qE '\blens-[0-9]'; then
    note "packages: lens visible"
  else
    fail "GHC does not expose lens; use the Nix with-packages toolchain"
  fi
else
  fail "no GHC on PATH"
fi

echo
if [ "$status" -eq 0 ]; then
  echo "== toolchain-doctor: OK =="
else
  echo "== toolchain-doctor: PROBLEMS FOUND ==" >&2
fi
exit "$status"
