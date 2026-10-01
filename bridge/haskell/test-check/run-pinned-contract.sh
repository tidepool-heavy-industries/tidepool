#!/usr/bin/env bash
# Native source check using the external module value from the owning facade
# test. Flake capture and resident host execution remain separate integration.
set -euo pipefail
if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift
support=${1:?generated effect module directory required}
scratch=${2:-target/async-wave/pinned-native}
mkdir -p "$scratch/Project" "$scratch/Ext" "$scratch/objects" "$scratch/cell-objects"
cp bridge/facade/src/exomonad/workspace_pinned_check.hs "$scratch/Project/Checks.hs"
cat > "$scratch/Ext/Tiny.hs" <<'HASKELL'
module Ext.Tiny where

tiny :: Int
tiny = 41
HASKELL
{
  git rev-parse HEAD
  command -v ghc
  ghc --version
  sha256sum bridge/facade/src/exomonad/workspace_pinned_check.hs "$scratch/Ext/Tiny.hs"
} > "$scratch/provenance.log"
ghc -O0 -Wall -i"$support" -i"$scratch" \
  -ibridge/haskell/lib -ibridge/haskell/actors \
  -outputdir "$scratch/objects" -o "$scratch/pinned-contract" \
  bridge/haskell/test-check/PinnedContract.hs > "$scratch/build.log" 2>&1
"$scratch/pinned-contract" "$support" "$scratch" | tee "$scratch/result.log"
