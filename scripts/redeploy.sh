#!/usr/bin/env bash
set -euo pipefail

# Always operate from the repo root — every path below (git status haskell/,
# cargo --path) assumes it.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DRY=0
NO_EXTRACT=0
NO_SERVERS=0

for arg in "$@"; do
  case "$arg" in
    --dry-run)    DRY=1 ;;
    --no-extract) NO_EXTRACT=1 ;;
    --no-servers) NO_SERVERS=1 ;;
    *) echo "error: unknown flag: $arg" >&2; exit 1 ;;
  esac
done

step() { echo; echo "==> $*"; }
run()  { echo "  \$ $*"; [ "$DRY" -eq 1 ] || "$@"; }

# Preflight: warn about conditions that cause silent deploy failures.

step "Preflight"

# A git-backed local flake includes TRACKED working-tree content (dirty or
# not) and EXCLUDES untracked files. So: untracked Haskell sources silently
# fail to ship (fatal), while tracked-dirty edits DO ship but make the build
# non-reproducible (informational).
haskell_untracked=$(git status --porcelain haskell/ 2>/dev/null | grep -E '^\?\?' || true)
if [ -n "$haskell_untracked" ]; then
  echo "ERROR: untracked files under haskell/ — the nix flake source EXCLUDES"
  echo "       untracked files, so these would silently not ship:"
  echo "$haskell_untracked" | sed 's/^/       /'
  echo "       git add them (or remove them) and re-run."
  exit 1
fi
haskell_dirty=$(git status --porcelain haskell/ 2>/dev/null | grep -vE '^[?!]{2}' || true)
if [ -n "$haskell_dirty" ]; then
  echo "note: haskell/ has uncommitted TRACKED changes — these WILL ship (the"
  echo "      flake sees the dirty working tree) but the build is not"
  echo "      reproducible from any commit; commit before deploys that matter."
fi

# Step 2: rebuild + install the GHC→Core extractor via nix profile.
#   Skippable with --no-extract (stdlib-only changes don't need this).

if [ "$NO_EXTRACT" -eq 0 ]; then
  step "Step 2: nix profile upgrade tidepool-extract"
  echo "  \$ nix profile upgrade tidepool-extract"
  if [ "$DRY" -eq 0 ]; then
    if ! nix profile upgrade tidepool-extract; then
      echo "hint: not yet in nix profile — install with:"
      echo "  nix profile install .#tidepool-extract"
      exit 1
    fi
    # Post-upgrade probe: a broken wrapper would otherwise surface only at
    # first eval. Same no-args `Usage:` banner check the test harness uses —
    # the banner is on stderr (stdout always carries the diagnostics JSON);
    # merge streams and let grep drain to EOF rather than truncating with
    # `head -c N` (a truncated read races the binary's second write, an EPIPE
    # there is an uncaught exception that fails the probe intermittently).
    # Probe the profile entry by ABSOLUTE path — `command -v` proves only that
    # something named tidepool-extract wins PATH, which may not be the entry
    # we just upgraded. Print the resolved store target so the deploy log
    # records which generation actually shipped.
    wrapper="$HOME/.nix-profile/bin/tidepool-extract"
    if [ ! -x "$wrapper" ]; then
      echo "error: $wrapper missing after upgrade" >&2
      exit 1
    fi
    echo "  wrapper resolves to: $(readlink -f "$wrapper")"
    if ! "$wrapper" 2>&1 | grep -q '^Usage:'; then
      echo "error: upgraded tidepool-extract does not print the 'Usage:' banner — broken wrapper" >&2
      exit 1
    fi
    if [ "$(command -v tidepool-extract)" != "$wrapper" ]; then
      echo "WARN: PATH resolves tidepool-extract to $(command -v tidepool-extract),"
      echo "      not the upgraded profile entry — runtime may use a different binary."
    fi
  fi
else
  echo; echo "(skipped: --no-extract)"
fi

# Steps 3+4: install Rust server binaries.
#   Skippable with --no-servers (extract-only changes don't need these).
#   Step 3 embeds the stdlib (haskell/lib/) into the binary at build time.

if [ "$NO_SERVERS" -eq 0 ]; then
  step "Step 3: cargo install tidepool (eval server + embedded stdlib)"
  run cargo install --path tidepool

  step "Step 4: cargo install tidepool-repl"
  run cargo install --path tidepool-repl
else
  echo; echo "(skipped: --no-servers)"
fi

# Step 5: clear stale CBOR + stdlib materialization cache.

step "Step 5: clear ~/.cache/tidepool/"
run rm -rf "${HOME}/.cache/tidepool/"

echo
echo "================================================================"
echo "done — now run /mcp reconnect in the Claude session"
echo "(the server processes are stale until reconnect)"
echo "================================================================"
