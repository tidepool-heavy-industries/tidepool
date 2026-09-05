#!/usr/bin/env bash
# One-command local Shoal bootstrap. Always use a matched worktree-built
# extractor frontend and Haskell compiler worker; an older installed wrapper
# must not leak into actor-policy compilation through PATH or inherited env.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# `just` preserves its option separator as the first variadic recipe
# argument. It separates Just flags, not Shoal's clap input.
if [[ "${1:-}" == "--" ]]; then
  shift
fi

unset TIDEPOOL_EXTRACT
unset TIDEPOOL_EXTRACT_WORKER
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

# Actor shells isolate Cargo artifacts under their actor build root. This
# bootstrap executes binaries from the workspace target directory, so own that
# path explicitly instead of inheriting a caller's CARGO_TARGET_DIR.
export CARGO_TARGET_DIR="$PWD/target"

source scripts/lib-extract.sh
resolve_tidepool_extract

echo "==> validating the local extractor/compiler endpoint"
validate_tidepool_extract_endpoint

echo "==> building Shoal"
cargo build -p tidepool --bin shoal

exec "$PWD/target/debug/shoal" init "$@"
