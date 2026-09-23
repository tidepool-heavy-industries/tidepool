#!/usr/bin/env bash
# Select the environment from committed inputs, never from mounted build trees.
set -euo pipefail

shell=default
if [[ ${1:-} == --exomonad ]]; then
  shell=exomonad
  shift
fi
if [[ $shell == exomonad ]]; then
  # Only exomonad's flake forces the codex path input, so only it needs the
  # vendor/codex source capture verified before Nix evaluates anything.
  bash scripts/codex-source-preflight.sh
fi

if [[ -n ${TIDEPOOL_DEV_FLAKE:-} ]]; then
  flake=$TIDEPOOL_DEV_FLAKE
  case "$flake" in
    git+*\?*rev=*|/nix/store/*) ;;
    *) echo 'TIDEPOOL_DEV_FLAKE must name a revision-pinned Git flake or immutable store path' >&2; exit 2 ;;
  esac
else
  common=$(git rev-parse --path-format=absolute --git-common-dir)
  if [[ $shell == default ]]; then
    # default only needs the toolchain inputs, so pin a synthetic commit over
    # them instead of HEAD: unrelated commits then reuse the same revision
    # instead of forcing Nix to re-fetch and re-evaluate the flake every time.
    if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml nix; then
      echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
      exit 2
    fi
    tree=$(git ls-tree HEAD -- flake.nix flake.lock rust-toolchain.toml nix | git mktree)
    revision=$(GIT_AUTHOR_DATE='@0 +0000' GIT_COMMITTER_DATE='@0 +0000' \
      GIT_AUTHOR_NAME=dev-shell GIT_AUTHOR_EMAIL=dev-shell@invalid \
      GIT_COMMITTER_NAME=dev-shell GIT_COMMITTER_EMAIL=dev-shell@invalid \
      git commit-tree "$tree" -m "dev-shell toolchain-input pin")
    if ! git rev-parse -q --verify "refs/tidepool/dev-shell/$revision" >/dev/null; then
      git update-ref "refs/tidepool/dev-shell/$revision" "$revision"
    fi
  else
    # exomonad forces the codex path input, which resolves against the real
    # worktree, so it stays pinned to HEAD rather than the reduced tree.
    if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml; then
      echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
      exit 2
    fi
    revision=$(git rev-parse HEAD)
  fi
  flake="git+file://$common?rev=$revision"
fi
selection="$flake#$shell"
if [[ ${TIDEPOOL_DEV_SHELL:-} == "$selection" &&
      -n ${TIDEPOOL_DEV_GHC:-} &&
      $(command -v ghc || true) == "$TIDEPOOL_DEV_GHC" &&
      $(command -v rustc || true) == "${TIDEPOOL_DEV_RUSTC:-}" ]]; then
  exec "$@"
fi
# Retain cwd and CARGO_TARGET_DIR: only the environment comes from the flake.
exec nix develop "$selection" --command bash -c '
  export TIDEPOOL_DEV_SHELL="$1"
  export TIDEPOOL_DEV_GHC="$(command -v ghc)"
  export TIDEPOOL_DEV_RUSTC="$(command -v rustc)"
  shift
  exec "$@"
' dev-shell "$selection" "$@"
