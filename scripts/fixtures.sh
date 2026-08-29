#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -ne 1 || ( "$1" != "check" && "$1" != "update" ) ]]; then
  echo "usage: $0 check|update" >&2
  exit 2
fi

fingerprint_file="haskell/test/suite_cbor/.source-fingerprint"

source_fingerprint() {
  {
    find haskell/src haskell/app haskell/lib tidepool-extract-cmd/src \
      -type f \( -name '*.hs' -o -name '*.rs' \) -print
    printf '%s\n' \
      flake.nix \
      flake.lock \
      rust-toolchain.toml \
      haskell/test/Suite.hs \
      haskell/tidepool-extract.cabal \
      haskell/cabal.project \
      tidepool-extract-cmd/Cargo.toml
  } | LC_ALL=C sort | xargs sha256sum | sha256sum | cut -d' ' -f1
}

generate() {
  local output_dir="$1"
  "$TIDEPOOL_EXTRACT" \
    haskell/test/Suite.hs \
    --all-closed \
    --include haskell/lib \
    --target-module-only \
    --output-dir "$output_dir"
}

if [[ "$1" == "update" ]]; then
  source scripts/lib-extract.sh
  resolve_tidepool_extract
  generate haskell/test/suite_cbor
  source_fingerprint >"$fingerprint_file"
  echo "updated haskell/test/suite_cbor"
  exit 0
fi

expected="$(source_fingerprint)"
actual="$(tr -d '[:space:]' <"$fingerprint_file" 2>/dev/null || true)"
if [[ "$actual" != "$expected" ]]; then
  echo "error: Haskell fixtures are stale; run: just fixtures-update" >&2
  exit 1
fi

cargo nextest run -p tidepool-eval --test haskell_suite \
  --status-level fail --final-status-level fail
echo "Haskell fixture fingerprint and semantic suite are current"
