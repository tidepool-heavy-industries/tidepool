#!/usr/bin/env bash
set -euo pipefail

# Always operate from the repo root — every path below (git status haskell/,
# the shim check, cargo --path) assumes it.
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

haskell_dirty=$(git status --porcelain haskell/ 2>/dev/null | grep -vE '^[?!]{2}' || true)
if [ -n "$haskell_dirty" ]; then
  echo "WARN: haskell/ has uncommitted tracked changes — nix flake build sees only"
  echo "      tracked files; uncommitted edits will NOT ship until committed."
fi

if [ ! -f .tidepool-repl-mcp.sh ]; then
  echo "WARN: .tidepool-repl-mcp.sh missing from repo root — tidepool-repl MCP will"
  echo "      ENOENT on connect. See tidepool-repl/CLAUDE.md (Launcher shim section)"
  echo "      for what it must contain. In brief, the shim must:"
  echo "        1. Prepend <nix-ghc-with-packages>/bin to PATH"
  echo "        2. Set TIDEPOOL_EXTRACT to the haskell/dist-newstyle cabal build output"
  echo "        3. exec ~/.cargo/bin/tidepool-repl \"\$@\""
  echo "      Then: cd haskell && cabal build tidepool-extract-bin"
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
    if ! command -v tidepool-extract >/dev/null 2>&1 \
       || ! tidepool-extract 2>&1 | grep -q '^Usage:'; then
      echo "error: installed tidepool-extract does not print the 'Usage:' banner — broken wrapper" >&2
      exit 1
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
