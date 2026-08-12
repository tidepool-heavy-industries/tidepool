#!/usr/bin/env bash
# Launch the self-iterating harness dogfood against an authored harness.
#   ./harness-dogfooding/run.sh                         # the open-ended companion
#   ./harness-dogfooding/run.sh path/to/Harness.hs      # any authored harness
# Rebuilds the extract + bin so it always runs the current tree. Model defaults
# to the OAuth (Codex) default baked into the bin; override with TIDEPOOL_LLM_MODEL.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

# The with-packages GHC (supplies ghc + lens/freer-simple to the extract
# subprocess); read from the deployed nix wrapper so it survives nix updates.
GHC="$(grep -oE '/nix/store/[^:"]*-with-packages/bin' "$HOME/.nix-profile/bin/tidepool-extract" | head -1)"
[ -n "$GHC" ] && export PATH="$GHC:$PATH"

echo "==> building extract + bin (fresh) ..."
( cd haskell && cabal build tidepool-extract-bin )
export TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
# Release is the dogfood profile (launch decision 2026-08-09): the JIT hot
# path is ~1.3-1.5x faster and it is the deployment being pitched.
# TIDEPOOL_PROFILE=debug overrides for stage-attribution work, whose
# historical tables were measured on debug.
PROFILE="${TIDEPOOL_PROFILE:-release}"
if [ "$PROFILE" = release ]; then
  cargo build -q --release -p tidepool-web --bin tidepool-selfharness
else
  cargo build -q -p tidepool-web --bin tidepool-selfharness
fi

HARNESS="${1:-harness-dogfooding/companion/Harness.hs}"
echo "==> launching: $HARNESS  ($PROFILE)  (open the printed 127.0.0.1 URL)"
export RUST_LOG="${RUST_LOG:-warn,tidepool_harness=debug,tidepool_web=debug}"
# Private dogfood cache (2026-08-08): 12 of 13 harness test files write the
# DEFAULT ~/.cache/tidepool/selfharness/, so any swarm test shard can poison a
# live session's checkpoint mid-run (it did, twice in one day — the
# StateDecode boot deaths). A dedicated XDG_CACHE_HOME makes dogfood state
# unreachable from tests regardless of what the box is running.
export XDG_CACHE_HOME="${TIDEPOOL_DOGFOOD_CACHE:-$HOME/.cache/tidepool-dogfood}"
echo "==> dogfood cache: $XDG_CACHE_HOME/tidepool/selfharness"
exec "./target/$PROFILE/tidepool-selfharness" --harness "$HARNESS"
