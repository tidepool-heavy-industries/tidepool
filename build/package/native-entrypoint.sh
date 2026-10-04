#!/usr/bin/env bash
set -euo pipefail
PACKAGE_BIN=$(cd -- "$(dirname -- "$0")" && pwd)
PACKAGE_ROOT=$(cd -- "$PACKAGE_BIN/.." && pwd)
export TIDEPOOL_EXTRACT="$PACKAGE_BIN/tidepool-extract"
export TIDEPOOL_EXTRACT_WORKER="$PACKAGE_BIN/tidepool-extract-bin"
export TIDEPOOL_COMPILER_DEPLOYMENT="$PACKAGE_ROOT/share/exomonad/compiler-deployment.json"
export TIDEPOOL_PRELUDE_DIR="$PACKAGE_ROOT/share/exomonad/stdlib"
IFS= read -r TIDEPOOL_GHC_LIBDIR < "$PACKAGE_ROOT/share/exomonad/ghc-libdir.txt"
export TIDEPOOL_GHC_LIBDIR
export EXOMONAD_EMBEDDED_ASSET_ROOT="$PACKAGE_ROOT/share/exomonad/web"
export EXOMONAD_WORKSPACE_GITLINK="$PACKAGE_ROOT/share/exomonad/workspace-gitlink.json"
export LD_LIBRARY_PATH="$PACKAGE_ROOT/lib/tidepool"
export PATH="$PACKAGE_ROOT/share/exomonad/runtime-tools/bin:$PACKAGE_BIN"
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET TIDEPOOL_COMPILER_MODULES TIDEPOOL_EXTRACT_NO_DAEMON
exec "$PACKAGE_BIN/exomonad-unwrapped" "$@"
