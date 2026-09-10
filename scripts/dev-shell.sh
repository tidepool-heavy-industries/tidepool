#!/usr/bin/env bash
# Select the environment from committed inputs, never from mounted build trees.
set -euo pipefail

shell=default
if [[ ${1:-} == --shoal ]]; then
  shell=shoal
  shift
fi
if [[ -n ${TIDEPOOL_DEV_FLAKE:-} ]]; then
  flake=$TIDEPOOL_DEV_FLAKE
  case "$flake" in
    git+*\?*rev=*|/nix/store/*) ;;
    *) echo 'TIDEPOOL_DEV_FLAKE must name a revision-pinned Git flake or immutable store path' >&2; exit 2 ;;
  esac
else
  if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml; then
    echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
    exit 2
  fi
  revision=$(git rev-parse HEAD)
  common=$(git rev-parse --path-format=absolute --git-common-dir)
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
