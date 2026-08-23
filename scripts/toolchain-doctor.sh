#!/usr/bin/env bash
# One command, one answer: "is the extract toolchain this worktree would use
# actually the one this worktree's Haskell source produces?"
#
# Ambient extractors are the #1 dev-UX hazard (bit three times in one week):
# a stale installed `tidepool-extract`, or one built against the wrong GHC,
# runs cleanly through the cheap `Usage:` probe `lib-extract.sh`/
# `eval_harness::extract_env` already do, then fails deep in a GHC-heavy test
# with an unrelated-looking error (`posix_spawnp: does not exist`, missing
# `Control.Lens`, or a stale CBOR decoder mismatch) — or, worse, compiles
# successfully against OLD translation logic and passes or fails for the
# wrong reason. This script answers the identity question up front:
#
#   - which binary would `TIDEPOOL_EXTRACT` resolve to right now;
#   - is it NEWER than every file under this worktree's haskell/{src,lib}
#     (a cheap mtime proxy for "built from this worktree's current source" —
#     see the nix-store caveat below for where this proxy does not apply);
#   - does GHC on PATH actually have the with-packages capability
#     (`lens` visible) the extractor needs at runtime — only load-bearing
#     for a directly-built binary; the deployed nix wrapper re-execs with
#     its own hard-coded with-packages PATH and needs no ambient help;
#   - the exact repair command, if any of the above is wrong.
#
# Usage: scripts/toolchain-doctor.sh
# Exit 0: extract resolved, fresh (or staleness explicitly allowed/not
#         applicable), and GHC capable. Exit 1: any of those is wrong — the
#         report above says which.
#
# TIDEPOOL_ALLOW_STALE_EXTRACT=1 downgrades a stale-binary finding from a
# failing exit to a warning — the same escape hatch `lib-extract.sh`'s
# `resolve_tidepool_extract` honors, for a deliberate cross-worktree run (a
# binary built by a DIFFERENT worktree is not "stale" from that worktree's
# own point of view, and this script has no way to tell the two apart from
# mtimes alone).
#
# NIX-STORE CAVEAT: `nix` canonicalizes every store path's mtime to a fixed
# epoch (observed: 1 second past the Unix epoch) for reproducibility, so the
# deployed `~/.nix-profile/bin/tidepool-extract` wrapper (and the store
# binary it execs) NEVER carries a meaningful build timestamp — comparing it
# against worktree source mtimes would be a permanent false "STALE". This
# script detects that shape (an implausibly old mtime) and reports staleness
# as "not applicable" instead, pointing at the deploy stamp
# (`tidepool --write-toolchain-stamp` / `scripts/redeploy.sh`) as the real
# cross-check for that binary.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo_root="$PWD"
nix_wrapper="$HOME/.nix-profile/bin/tidepool-extract"
# Anything at or before this looks like a nix store's canonicalized mtime,
# not a real build time (observed: epoch+1s). Generous cutoff (year 2000).
plausible_mtime_floor=946684800

status=0
note() { echo "  $*"; }
warn() { echo "  WARNING: $*" >&2; }
fail() { echo "  ERROR: $*" >&2; status=1; }

echo "== tidepool-extract selection =="
extract_bin=""
extract_source=""
is_nix_wrapper=0
if [ -n "${TIDEPOOL_EXTRACT:-}" ]; then
  extract_bin="$TIDEPOOL_EXTRACT"
  extract_source="\$TIDEPOOL_EXTRACT (env override)"
  [ "$extract_bin" = "$nix_wrapper" ] && is_nix_wrapper=1
elif bin="$(cd haskell && cabal list-bin tidepool-extract-bin 2>/dev/null)" && [ -n "$bin" ] && [ -x "$bin" ]; then
  extract_bin="$bin"
  extract_source="cabal dev build (haskell/dist-newstyle, this worktree)"
elif command -v tidepool-extract >/dev/null 2>&1; then
  extract_bin="$(command -v tidepool-extract)"
  extract_source="\$PATH (typically ~/.nix-profile/bin/tidepool-extract)"
  is_nix_wrapper=1
fi

if [ -z "$extract_bin" ]; then
  fail "no tidepool-extract resolvable at all (checked \$TIDEPOOL_EXTRACT, cabal list-bin, \$PATH)"
  echo
  echo "== repair =="
  note "cd haskell && cabal build tidepool-extract-bin"
  note "export TIDEPOOL_EXTRACT=\$(cd haskell && cabal list-bin tidepool-extract-bin)"
  exit 1
fi

note "selected:    $extract_bin"
note "source:      $extract_source"

if [ ! -x "$extract_bin" ]; then
  fail "selected extract binary is not executable: $extract_bin"
else
  fingerprint="$(sha256sum "$extract_bin" 2>/dev/null | cut -c1-16)"
  mtime_epoch="$(stat -c %Y "$extract_bin" 2>/dev/null || echo 0)"
  mtime_human="$(stat -c %y "$extract_bin" 2>/dev/null || echo unknown)"
  note "fingerprint: sha256:${fingerprint:-unknown}..."
  note "built:       $mtime_human"

  echo
  echo "== worktree identity (mtime proxy, not a source digest) =="
  newest_src_epoch="$(find "$repo_root/haskell/src" "$repo_root/haskell/lib" -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1 | cut -d. -f1)"
  newest_src_epoch="${newest_src_epoch:-0}"
  newest_src_human="$(date -d "@$newest_src_epoch" 2>/dev/null || echo unknown)"
  note "newest haskell/{src,lib} file: $newest_src_human"

  if [ "$mtime_epoch" -le "$plausible_mtime_floor" ]; then
    note "STATUS: not applicable — this binary carries a nix-store canonicalized timestamp, not a real build time"
    note "cross-check freshness via the deploy stamp instead: 'tidepool --write-toolchain-stamp' was run at deploy time by scripts/redeploy.sh"
  elif [ "$mtime_epoch" -lt "$newest_src_epoch" ]; then
    if [ "${TIDEPOOL_ALLOW_STALE_EXTRACT:-0}" = "1" ]; then
      warn "extract binary is OLDER than the newest haskell/{src,lib} file — STALE relative to this worktree, continuing (TIDEPOOL_ALLOW_STALE_EXTRACT=1)"
    else
      fail "extract binary is OLDER than the newest haskell/{src,lib} file — your extract binary is stale, rebuild from this worktree"
      echo
      echo "== repair =="
      note "cd haskell && cabal build tidepool-extract-bin"
      note "export TIDEPOOL_EXTRACT=\$(cd haskell && cabal list-bin tidepool-extract-bin)"
      note "(or set TIDEPOOL_ALLOW_STALE_EXTRACT=1 if this is a deliberate cross-worktree/pinned run)"
    fi
  else
    note "STATUS: fresh (built after every current haskell/{src,lib} file)"
  fi

  echo
  echo "== runnable =="
  if usage_out="$("$extract_bin" 2>&1 1>/dev/null)" && echo "$usage_out" | grep -q '^Usage:'; then
    note "'$extract_bin' prints its Usage: banner — process spawns and runs"
  else
    fail "'$extract_bin' did not print a Usage: banner on a no-args invocation"
    note "this is the with-packages-GHC-missing class of failure — the with-packages GHC must be on PATH"
    echo
    echo "== repair =="
    note "check: which ghc   (must resolve to a *-with-packages nix derivation, not a bare system ghc)"
    note "the deployed wrapper hard-codes it: grep -oE '/nix/store/[^:\"]*-with-packages/bin' $nix_wrapper"
    note "export PATH=<that with-packages bin dir>:\$PATH"
  fi
fi

echo
echo "== GHC on PATH =="
if [ "$is_nix_wrapper" = 1 ]; then
  note "selected extract is the nix-profile wrapper — it re-execs with its OWN hard-coded with-packages GHC PATH,"
  note "so the ambient PATH's GHC is not load-bearing for it (the 'runnable' check above already proves the wrapper works)."
elif ghc_path="$(command -v ghc 2>/dev/null)"; then
  note "path:    $ghc_path"
  note "version: $(ghc --version 2>/dev/null || echo unknown)"
  if command -v ghc-pkg >/dev/null 2>&1 && ghc-pkg list 2>/dev/null | grep -qE '\blens-[0-9]'; then
    note "packages: lens visible (with-packages capability confirmed)"
  else
    fail "GHC on PATH does not expose 'lens' — this is a bare/system GHC, not the with-packages derivation tidepool-extract needs"
    echo
    echo "== repair =="
    note "the with-packages GHC must be on PATH — see $nix_wrapper's hard-coded store path:"
    note "  grep -oE '/nix/store/[^:\"]*-with-packages/bin' $nix_wrapper"
    note "  export PATH=<that dir>:\$PATH"
  fi
else
  fail "no 'ghc' on PATH at all"
  echo
  echo "== repair =="
  note "run inside 'nix develop', or put a with-packages GHC on PATH"
fi

echo
if [ "$status" -eq 0 ]; then
  echo "== toolchain-doctor: OK =="
else
  echo "== toolchain-doctor: PROBLEMS FOUND (see ERROR lines above) ==" >&2
fi
exit "$status"
