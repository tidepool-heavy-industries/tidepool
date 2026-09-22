#!/usr/bin/env bash
set -euo pipefail

# Always operate from the repo root — every path below (git status bridge/haskell/,
# cargo --path) assumes it.
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib-extract.sh

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
#
# Excludes `bridge/haskell/dist-newstyle*` — cabal's own build-output dirs (the
# plain `dist-newstyle/` name is .gitignore'd already; concurrent agents on a
# shared box sometimes run cabal with a differently-suffixed `--builddir`,
# e.g. `dist-newstyle-schema10/`). These are build OUTPUTS, never deploy
# SOURCE inputs the flake would need to ship, so their being untracked is not
# the silent-failure case this check exists to catch.
haskell_untracked=$(git status --porcelain bridge/haskell/ 2>/dev/null | grep -E '^\?\?' | grep -vE '^\?\? bridge/haskell/dist-newstyle' || true)
if [ -n "$haskell_untracked" ]; then
  echo "ERROR: untracked files under bridge/haskell/ — the nix flake source EXCLUDES"
  echo "       untracked files, so these would silently not ship:"
  echo "$haskell_untracked" | sed 's/^/       /'
  echo "       git add them (or remove them) and re-run."
  exit 1
fi
haskell_dirty=$(git status --porcelain bridge/haskell/ 2>/dev/null | grep -vE '^[?!]{2}' || true)
if [ -n "$haskell_dirty" ]; then
  echo "note: bridge/haskell/ has uncommitted TRACKED changes — these WILL ship (the"
  echo "      flake sees the dirty working tree) but the build is not"
  echo "      reproducible from any commit; commit before deploys that matter."
fi

# Step 2: rebuild + install the compiler worker via nix profile.
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
    if ! extract_has_usage_banner "$wrapper"; then
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
#   Step 3 embeds the stdlib (bridge/haskell/lib/) into the binary at build time.

if [ "$NO_SERVERS" -eq 0 ]; then
  # --locked: install from the workspace Cargo.lock instead of re-resolving —
  # a fresh resolution can fail on yanked-but-locked deps (seen live:
  # arrayref 0.3.x) and would silently deploy different dep versions than the
  # tree that passed the test suite.
  step "Step 3: cargo install tidepool (Shoal + embedded Haskell)"
  run cargo install --locked --path tidepool
else
  echo; echo "(skipped: --no-servers)"
fi

# Step 5: clear stale CBOR + stdlib materialization cache.
#
#   Scoped to exactly the toolchain/compile-cache paths a redeploy can make
#   stale (tidepool-toolchain::paths — see its module doc's "regenerable
#   cache" scope, and cache.rs's key-file layout): the materialized stdlib
#   (`stdlib/`), the generated `Tidepool.Effects` module (`effects/`), the
#   module-granular GHC interface cache (`build-products/`), and the
#   content-addressed compile-cache key files that cache.rs writes as loose
#   files directly under the cache root (`<key>.ok`, `<key>.cbor` and its
#   `.meta`/`.prepared` variants, `<key>.asks.json`, `<key>.a<N>`).
#
#   Deliberately NOT a wholesale `rm -rf ~/.cache/tidepool/`: that root also
#   holds `actor-builds/`, `shoal/` (Shoal actor-worktree state), and
#   `toolchain-stamp.json` itself
#   — none of which a redeploy invalidates, and on a shared box the worktree
#   state under `shoal/actor-worktrees/**` belongs to OTHER agents' live
#   work. `toolchain-stamp.json` is left alone here too: Step 6 overwrites it
#   atomically regardless of its prior content, so there is nothing to clear
#   pre-emptively, and the old MUST-run-after-Step-5 ordering concern
#   (deleting a stamp Step 6 just wrote) no longer applies.

step "Step 5: clear the toolchain/compile-cache subdirectories of ~/.cache/tidepool/"
cache_dir="${HOME}/.cache/tidepool"
run rm -rf "${cache_dir}/stdlib" "${cache_dir}/effects" "${cache_dir}/build-products"
if [ "$DRY" -eq 1 ]; then
  echo "  \$ find ${cache_dir} -maxdepth 1 -type f \\( -name '*.ok' -o -name '*.cbor' -o -name '*.asks.json' -o -name '*.a[0-9]*' -o -name 'binfp-*' \\) -delete"
else
  find "${cache_dir}" -maxdepth 1 -type f \
    \( -name '*.ok' -o -name '*.cbor' -o -name '*.asks.json' -o -name '*.a[0-9]*' -o -name 'binfp-*' \) \
    -delete 2>/dev/null || true
fi

# Step 6: write the toolchain deploy stamp — content fingerprints of the
#   extract + stdlib just deployed, checked by every server at startup
#   (tidepool/toolchain/src/toolchain.rs). Runs after Step 5 by convention
#   (mirrors the deploy order: invalidate stale cache, then bless the fresh
#   pair), though Step 5 no longer touches toolchain-stamp.json, so ordering
#   between them is no longer load-bearing.
#   Skipped when --no-servers was passed: the tidepool binary this stamp
#   describes was not (re)installed this run, so there is nothing fresh to
#   fingerprint. Call by ABSOLUTE path (do not trust PATH), same discipline
#   as Step 2's wrapper probe.

step "Step 6: write toolchain deploy stamp"

if [ "$NO_SERVERS" -eq 1 ]; then
  echo "  (skipped: --no-servers — the tidepool binary was not installed this run)"
else
  stamp_bin="$HOME/.cargo/bin/tidepool"
  echo "  \$ $stamp_bin --write-toolchain-stamp"
  if [ "$DRY" -eq 0 ]; then
    if [ ! -x "$stamp_bin" ]; then
      echo "error: $stamp_bin missing — expected Step 3 (cargo install --path tidepool) to have installed it" >&2
      exit 1
    fi
    if ! "$stamp_bin" --write-toolchain-stamp; then
      echo "error: writing the toolchain deploy stamp failed — extract and stdlib are deployed but" >&2
      echo "       servers cannot prove they were deployed together; see bridge/haskell/CLAUDE.md's" >&2
      echo "       Deploy handshake section" >&2
      exit 1
    fi
  fi
fi

echo
echo "================================================================"
echo "done — deploy stamp written; now run /mcp reconnect in the Claude session"
echo "(the server processes are stale until reconnect)"
echo "================================================================"
