#!/usr/bin/env bash
# Run ONE GHC-heavy crate's tests, as a survivable shard of the full battery.
#
# `scripts/battery.sh` runs the ENTIRE workspace (`--ignore-default-filter`)
# in one process, which is fine on a quiet dedicated machine but is
# hours-long here and gets hard-killed by this environment's ~380s
# background-process cap long before it finishes. Splitting by crate makes
# full coverage achievable as a sequence of shards, each of which fits under
# that cap (modulo the TIDEPOOL_EXPENSIVE_TESTS=1 suites — see below).
#
# Usage: scripts/battery-shard.sh <crate> [extra nextest args...]
#   scripts/battery-shard.sh tidepool-runtime
#   scripts/battery-shard.sh tidepool-codegen -E 'binary(proptest_ghc_idioms)'
#
# This does NOT set TIDEPOOL_EXPENSIVE_TESTS — the multi-hundred-second
# suites (lazy_consumption_property_suite, effectful_lazy_ab_x8,
# corpus_report, haskell_suite_differential, tidepool-testing::haskell_verified)
# stay skipped unless you export TIDEPOOL_EXPENSIVE_TESTS=1 yourself. Run
# those deliberately, one at a time, with their own budget — they are NOT
# what this script's ~380s-per-shard promise covers.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ $# -lt 1 ]; then
  echo "usage: $0 <crate> [extra nextest args...]" >&2
  exit 1
fi
crate="$1"
shift

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
  echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
  # The locally-built binary needs the with-packages GHC (supplying lens/
  # freer-simple) on PATH at runtime, or extraction fails with "Could not find
  # module Control.Lens". The deployed nix wrapper hard-codes that GHC's path;
  # reuse it so a bare `nix develop` run works without manual PATH surgery.
  _w="$HOME/.nix-profile/bin/tidepool-extract"
  if [ -x "$_w" ]; then
    _ghc="$(grep -oE '/nix/store/[^:"]*-with-packages/bin' "$_w" | head -1)"
    if [ -n "${_ghc:-}" ] && [ -d "$_ghc" ]; then
      export PATH="$_ghc:$PATH"
      echo "==> prepended with-packages GHC to PATH ($_ghc)"
    fi
  fi
  ( cd haskell && cabal build tidepool-extract-bin )
  # Split assignment from export: `export VAR="$(cmd)"` masks the command's
  # exit status (SC2155), so a failed list-bin would proceed with an empty var.
  TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
  export TIDEPOOL_EXTRACT
fi

# Same announced-binary sanity probe as battery.sh — see that script for why
# the checks are shaped this way (silent extract_env fallback, EPIPE hazard).
if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
  echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
  exit 1
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
echo "==> shard: -p ${crate} (--ignore-default-filter, TIDEPOOL_EXPENSIVE_TESTS=${TIDEPOOL_EXPENSIVE_TESTS:-unset})"

exec cargo nextest run --ignore-default-filter -p "$crate" "$@"
