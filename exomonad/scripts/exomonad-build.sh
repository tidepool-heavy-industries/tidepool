#!/usr/bin/env bash
# Build a matched Exomonad, extractor frontend, and compiler worker from this
# checkout. The Just recipes enter the Nix development shell before calling us.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Actor shells may set a separate Cargo target directory. The local entry
# points execute target/debug/exomonad from this checkout.
export CARGO_TARGET_DIR="$PWD/target"
unset TIDEPOOL_EXTRACT
unset TIDEPOOL_EXTRACT_WORKER
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

source scripts/lib-extract.sh
resolve_tidepool_extract

echo "==> validating the local extractor/compiler endpoint"
validate_tidepool_extract_endpoint

echo "==> building Exomonad"
cargo build -p tidepool --bin exomonad
