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
# The session extract loads Tidepool.Prelude (transitive lens), so it needs the
# WITH-PACKAGES GHC libdir, NOT the bare dev-shell ghc (`ghc --print-libdir`).
# Pick the first with-packages store that actually carries a lens conf.
TIDEPOOL_GHC_LIBDIR="$(for d in /nix/store/*ghc-native-bignum-9.12.2-with-packages/lib/ghc-9.12.2/lib; do ls "$d/package.conf.d" 2>/dev/null | grep -qi '^lens-' && { echo "$d"; break; }; done)"
set +a
echo "frontend: $TIDEPOOL_EXTRACT"
echo "worker:   ${TIDEPOOL_EXTRACT_WORKER:-MISSING (run: cd haskell && cabal build tidepool-extract-bin)}"
echo "libdir:  $TIDEPOOL_GHC_LIBDIR"
