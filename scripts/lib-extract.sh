#!/usr/bin/env bash
# Shared TIDEPOOL_EXTRACT resolution + validation, sourced by battery.sh,
# battery-shard.sh, and bench-turn.sh. One copy so the three callers can't
# drift out of sync with each other.
#
# Usage: source this file, then call `resolve_tidepool_extract`. Callers must
# already be cd'd to the repo root (all three do this before sourcing) since
# the build step below runs `cd haskell`.

resolve_tidepool_extract() {
  # Captured BEFORE the build-if-unset branch below: the staleness check
  # after it only applies to a caller-SUPPLIED TIDEPOOL_EXTRACT — a binary
  # this function just built itself is trivially fresh (see
  # scripts/toolchain-doctor.sh, which runs the identical check standalone
  # and explains the nix-store mtime caveat this skips around).
  local _was_preset=0
  [ -n "${TIDEPOOL_EXTRACT:-}" ] && _was_preset=1

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

  # A caller-supplied TIDEPOOL_EXTRACT skipped the build-above branch
  # entirely, so it never got the freshness this function gives its own
  # builds for free — this is the stale-ambient-extract class (bit three
  # times in one week): a binary that runs cleanly through the Usage: probe
  # below but was compiled before a subsequent haskell/{src,lib} edit, then
  # fails deep in a GHC-heavy test with an unrelated-looking error, or
  # silently exercises old translation logic. `nix` canonicalizes every store
  # path's mtime to a fixed near-epoch value for reproducibility (this covers
  # both a literal /nix/store/... path and the ~/.nix-profile/bin wrapper
  # symlink chain, whose resolved mtime is the same), so a binary whose mtime
  # predates the year 2000 is treated as "not applicable" rather than a false
  # STALE — see toolchain-doctor.sh's matching caveat for the full reasoning.
  if [ "$_was_preset" = 1 ] && [ -x "$TIDEPOOL_EXTRACT" ]; then
    _bin_mtime="$(stat -c %Y "$TIDEPOOL_EXTRACT" 2>/dev/null || echo 0)"
    _plausible_mtime_floor=946684800 # year 2000 — below this looks nix-canonicalized, not a real build time
    if [ "$_bin_mtime" -gt "$_plausible_mtime_floor" ]; then
      _newest_src="$(find "$PWD/haskell/src" "$PWD/haskell/lib" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
      _newest_src="${_newest_src:-0}"
      if [ "$_bin_mtime" -lt "$_newest_src" ]; then
        if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" = "1" ]; then
          echo "warning: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than the newest haskell/{src,lib} file — continuing (TIDEPOOL_ALLOW_STALE_EXTRACT=1)" >&2
        else
          echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is older than the newest haskell/{src,lib} file — your extract binary is stale, rebuild from this worktree" >&2
          echo "  fix: cd haskell && cabal build tidepool-extract-bin && export TIDEPOOL_EXTRACT=\$(cd haskell && cabal list-bin tidepool-extract-bin)" >&2
          echo "  or, for a deliberate cross-worktree/pinned run: TIDEPOOL_ALLOW_STALE_EXTRACT=1" >&2
          echo "  (full diagnostic: scripts/toolchain-doctor.sh)" >&2
          exit 1
        fi
      fi
    fi
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
