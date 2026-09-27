#!/usr/bin/env bash
# Build a matched Exomonad, extractor frontend, and compiler worker from this
# checkout. The Just recipes enter the Nix development shell before calling us.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Actor shells may set a separate Cargo target directory. The local entry
# points execute target/debug/exomonad from this checkout.
export CARGO_TARGET_DIR="$PWD/target"
# A caller can select this checkout's already-built matched producer. Require
# both halves and keep their paths under their owning build trees; arbitrary
# inherited binaries from another actor checkout are not this build's tools.
# The existing explicit stale override remains available for deliberate
# cross-checkout work, and resolve_tidepool_extract still validates both halves.
if [ -n "${TIDEPOOL_EXTRACT:-}" ] || [ -n "${TIDEPOOL_EXTRACT_WORKER:-}" ]; then
  if [ -z "${TIDEPOOL_EXTRACT:-}" ] || [ -z "${TIDEPOOL_EXTRACT_WORKER:-}" ]; then
    echo "error: set both TIDEPOOL_EXTRACT and TIDEPOOL_EXTRACT_WORKER for an explicit producer" >&2
    exit 1
  fi
  if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" != 1 ]; then
    frontend="$(realpath -m -- "$TIDEPOOL_EXTRACT")"
    worker="$(realpath -m -- "$TIDEPOOL_EXTRACT_WORKER")"
    case "$frontend" in
      "$PWD"/target/*) ;;
      *) echo "error: explicit extractor is outside this checkout's target tree" >&2; exit 1 ;;
    esac
    case "$worker" in
      "$PWD"/bridge/haskell/dist-newstyle/*) ;;
      *) echo "error: explicit compiler worker is outside this checkout's Cabal tree" >&2; exit 1 ;;
    esac
  fi
else
  unset TIDEPOOL_EXTRACT
  unset TIDEPOOL_EXTRACT_WORKER
fi
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

source scripts/lib-extract.sh
resolve_tidepool_extract

echo "==> validating the local extractor/compiler endpoint"
validate_tidepool_extract_endpoint

echo "==> building Exomonad"
cargo build -p tidepool --bin exomonad
