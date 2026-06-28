#!/usr/bin/env bash
# Sourced harness for tidepool-repl session tests. Points cargo tests at THIS
# worktree's session-aware tidepool-extract + the with-packages GHC libdir.
# Worktree-portable: resolves the extract from THIS checkout's dist-newstyle via
# `cabal list-bin`, so a git worktree uses its OWN built extract (build it first:
#   ( cd "$(git rev-parse --show-toplevel)/haskell" && cabal build tidepool-extract-bin )
# ). Run tests via:
#   nix develop --command bash -lc '. .session-test-env.sh && cargo test ...'
set -a
REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
TIDEPOOL_EXTRACT="$(cd "$REPO/haskell" 2>/dev/null && cabal list-bin tidepool-extract-bin 2>/dev/null)"
# The session extract loads Tidepool.Prelude (transitive lens), so it needs the
# WITH-PACKAGES GHC libdir, NOT the bare dev-shell ghc (`ghc --print-libdir`).
# Pick the first with-packages store that actually carries a lens conf.
TIDEPOOL_GHC_LIBDIR="$(for d in /nix/store/*ghc-native-bignum-9.12.2-with-packages/lib/ghc-9.12.2/lib; do ls "$d/package.conf.d" 2>/dev/null | grep -qi '^lens-' && { echo "$d"; break; }; done)"
set +a
echo "extract: ${TIDEPOOL_EXTRACT:-MISSING (run: cd haskell && cabal build tidepool-extract-bin)}"
echo "libdir:  $TIDEPOOL_GHC_LIBDIR"
