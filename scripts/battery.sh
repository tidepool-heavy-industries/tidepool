#!/usr/bin/env bash
# The full workspace test battery, via cargo-nextest.
#
# nextest runs every test in its OWN process (never two tests sharing one),
# which structurally de-races the JIT's process-global-ish state (signal
# handlers, GC, fork-safety harnesses) that the old `-- --test-threads=1`
# discipline serialized against by brute force. See .config/nextest.toml for
# the hazard-audit note and repo-root CLAUDE.md's Build & Test section.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
  echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
  ( cd haskell && cabal build tidepool-extract-bin )
  export TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"

# --ignore-default-filter: the full battery runs EVERY crate, including the
# GHC-extract-heavy ones that .config/nextest.toml's default-filter skips for
# quick inner-loop `cargo nextest run`. Same profile, so slow-timeout + the
# ghc-heavy thread cap still apply.
exec cargo nextest run --workspace --ignore-default-filter "$@"
