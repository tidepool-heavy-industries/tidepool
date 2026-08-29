#!/usr/bin/env bash
# Sourced harness for tidepool-repl session tests. Points cargo tests at THIS
# worktree's Rust extractor frontend, Haskell compiler worker, and GHC libdir.
# Worktree-portable: both binaries come from THIS checkout (build them first:
#   cargo build -p tidepool-extract-cmd --bin tidepool-extract
#   ( cd "$(git rev-parse --show-toplevel)/haskell" && cabal build tidepool-extract-bin )
# ). Run tests via:
#   nix develop --command bash -lc '. .session-test-env.sh && cargo test ...'
set -a
REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
TIDEPOOL_EXTRACT="$REPO/target/debug/tidepool-extract"
TIDEPOOL_EXTRACT_WORKER="$(cd "$REPO/haskell" 2>/dev/null && cabal list-bin tidepool-extract-bin 2>/dev/null)"
# `nix develop` exposes the same with-packages compiler used by the deployed
# extractor, so its libdir is authoritative and worktree-portable.
TIDEPOOL_GHC_LIBDIR="${TIDEPOOL_GHC_LIBDIR:-$(ghc --print-libdir)}"
set +a
echo "frontend: $TIDEPOOL_EXTRACT"
echo "worker:   ${TIDEPOOL_EXTRACT_WORKER:-MISSING (run: cd haskell && cabal build tidepool-extract-bin)}"
echo "libdir:  $TIDEPOOL_GHC_LIBDIR"
