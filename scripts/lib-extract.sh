#!/usr/bin/env bash
# Shared TIDEPOOL_EXTRACT resolution + validation, sourced by battery.sh,
# battery-shard.sh, and bench-turn.sh. One copy so the three callers can't
# drift out of sync with each other.
#
# Usage: source this file, then call `resolve_tidepool_extract`. Callers must
# already be cd'd to the repo root (all three do this before sourcing) since
# the build step below runs `cd haskell`.

resolve_tidepool_extract() {
  if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
    echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
    # The locally-built binary needs the with-packages GHC (supplying lens/
    # freer-simple) on PATH at runtime, or extraction fails with "Could not
    # find module Control.Lens". The deployed nix wrapper hard-codes that
    # GHC's path; reuse it so a bare `nix develop` run works without manual
    # PATH surgery.
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
    # exit status (SC2155), so a failed list-bin would proceed with an empty
    # var.
    TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
    export TIDEPOOL_EXTRACT
  fi

  # The announced binary must actually run. eval_harness::extract_env
  # silently falls back to a `cabal list-bin` binary when the announced one
  # doesn't execute (so the banner below could name a binary the tests never
  # used), and when nothing resolves the GHC-guarded suites "skip cleanly" —
  # a green battery with zero GHC coverage. Same no-args `Usage:` probe
  # extract_env uses — the banner is on stderr, written before stdout's
  # diagnostics JSON, so a plain merged `2>&1` (not a stdout/stderr swap)
  # sees it first either way. Do NOT truncate the read with `head -c N`: the
  # extract binary ALWAYS writes a second thing after the banner (the
  # diagnostics JSON, to stdout) — `head` closing the pipe the instant it has
  # its N bytes races that second write, and an EPIPE there is an uncaught
  # exception that fails the process (an intermittent nonzero exit with no
  # other symptom). `grep` alone drains the pipe to EOF, so the writer never
  # gets closed out from under it.
  if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
    echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
    exit 1
  fi
  echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
}
