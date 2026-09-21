#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -ne 1 || ( "$1" != "check" && "$1" != "update" ) ]]; then
  echo "usage: $0 check|update" >&2
  exit 2
fi

source scripts/lib-extract.sh
resolve_tidepool_extract

# This compact constructor set covers the non-wired Haskell data used while
# comparing the Suite corpus. Keeping metadata generation here makes
# `fixtures-update` an actual update operation and makes `fixtures-check`
# reject a stale metadata wire version before the expensive corpus run.
fixture_targets="con_left,con_right,con_just,con_nothing,showInt"
fixture_tmp="$(mktemp -d target/fixture-metadata.XXXXXX)"
trap 'rm -rf "$fixture_tmp"' EXIT
"$TIDEPOOL_EXTRACT" haskell/test/Suite.hs \
  --targets "$fixture_targets" \
  --include haskell/lib \
  --target-module-only \
  --output-dir "$fixture_tmp"

fixture_metadata="haskell/test/suite_cbor/meta.cbor"
if [[ "$1" == update ]]; then
  cp "$fixture_tmp/meta.cbor" "$fixture_metadata"
elif ! cmp -s "$fixture_tmp/meta.cbor" "$fixture_metadata"; then
  echo "error: $fixture_metadata is stale; run 'just fixtures-update'" >&2
  exit 1
fi

scripts/prepared-corpus.sh
