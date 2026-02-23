#!/usr/bin/env bash
# Development wrapper for tidepool-extract that uses the locally-built harness
# with the threshold-bumped GHC from the nix overlay.
#
# The nix-installed ghc-with-packages doesn't have fat interfaces, so we use
# the overlay GHC directly. freer-simple is found via cabal's package DB.
#
# Usage: TIDEPOOL_EXTRACT=./haskell/tidepool-extract-dev.sh cargo test ...

DIR="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$DIR/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-harness-0.1.0.0/x/tidepool-harness/build/tidepool-harness/tidepool-harness"

if [ ! -x "$HARNESS" ]; then
    echo "Error: local harness not found at $HARNESS" >&2
    echo "Run: nix develop --command bash -c 'cd haskell && cabal build tidepool-harness'" >&2
    exit 1
fi

# Use the threshold-bumped GHC from our nix overlay
OVERLAY_GHC="/nix/store/x56y45disdf7vp26d475zhghz7c6z5cd-ghc-9.12.2"

# Also need the nix ghc-with-packages for freer-simple package registration
NIX_GHC_WITH_PKGS="/nix/store/9bf6g6g4md890kqy5ymrqk05ycm61g1h-ghc-9.12.2-with-packages"

# Overlay GHC first (for fat interfaces), then ghc-with-packages (for freer-simple)
export PATH="$OVERLAY_GHC/bin:$NIX_GHC_WITH_PKGS/bin:$PATH"

# Point GHC at the overlay's package DB which has the fat interfaces
export GHC_PACKAGE_PATH="$OVERLAY_GHC/lib/ghc-9.12.2/lib/package.conf.d:$NIX_GHC_WITH_PKGS/lib/ghc-9.12.2/lib/package.conf.d"

exec "$HARNESS" "$@"
