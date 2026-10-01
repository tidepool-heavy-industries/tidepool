#!/usr/bin/env bash
# The caller supplies its exact matched Cargo dependency directory and --extern
# selections; this compiles only the fixtures and does not launch a daemon.
set -euo pipefail
if [[ ${1:-} != --in-dev-shell ]]; then
  exec bash scripts/dev-shell.sh bash "$0" --in-dev-shell "$@"
fi
shift
dependencies=${1:?matched Cargo dependency directory required}
output=${2:?isolated fixture output directory required}
shift 2
mkdir -p "$output"
rustc --edition=2021 bridge/haskell/test-check/PreparedContract.rs \
  -L dependency="$dependencies" "$@" -o "$output/prepared-contract"
rustc --edition=2021 bridge/haskell/test-check/CaptureRecipes.rs \
  -L dependency="$dependencies" "$@" -o "$output/capture-recipes"
rustc --edition=2021 bridge/haskell/test-check/EffectSupport.rs \
  -L dependency="$dependencies" "$@" -o "$output/effect-support"
