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
  # Split assignment from export: `export VAR="$(cmd)"` masks the command's
  # exit status (SC2155), so a failed list-bin would proceed with an empty var.
  TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
  export TIDEPOOL_EXTRACT
fi

# The announced binary must actually run. eval_harness::extract_env silently
# falls back to a `cabal list-bin` binary when the announced one doesn't
# execute (so the banner below could name a binary the tests never used), and
# when nothing resolves the GHC-guarded suites "skip cleanly" — a green
# battery with zero GHC coverage. Same no-args `Usage:` probe extract_env uses
# — the banner is on stderr, written before stdout's diagnostics JSON, so a
# plain merged `2>&1` (not a stdout/stderr swap) sees it first either way.
# The swap idiom (`2>&1 1>/dev/null`) is unreliable here: when THIS script's
# own stdout+stderr are already redirected to one file by the caller (e.g. a
# backgrounded `battery.sh >log 2>&1`), re-swapping them for a nested pipeline
# can silently misroute the child's output — reproduces with plain `echo`,
# nothing GHC-specific. Merging avoids the swap entirely.
if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | head -c 6 | grep -q 'Usage:'; then
  echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
  exit 1
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"

# --ignore-default-filter: the full battery runs EVERY crate, including the
# GHC-extract-heavy ones that .config/nextest.toml's default-filter skips for
# quick inner-loop `cargo nextest run`. Same profile, so slow-timeout + the
# ghc-heavy thread cap still apply.
exec cargo nextest run --workspace --ignore-default-filter "$@"
