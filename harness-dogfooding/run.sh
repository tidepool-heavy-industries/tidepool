#!/usr/bin/env bash
# Launch the self-iterating harness dogfood against an authored harness.
#   ./harness-dogfooding/run.sh                         # the feature-brainstorm wizard
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
cargo build -q -p tidepool-web --bin tidepool-selfharness

HARNESS="${1:-harness-dogfooding/wizard/Harness.hs}"
echo "==> launching: $HARNESS  (open the printed 127.0.0.1 URL)"
exec ./target/debug/tidepool-selfharness --harness "$HARNESS"
