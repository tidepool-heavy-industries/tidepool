#!/usr/bin/env bash
# Build this checkout and run one Exomonad subcommand (init, new, check, ...)
# with its matched local extractor and compiler worker. Bare
# `target/debug/exomonad` would resolve `tidepool-extract` from $PATH, where an
# installed copy from another checkout can shadow the one just built.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# `just` preserves its option separator as the first variadic recipe
# argument. It separates Just flags, not Exomonad's clap input.
if [[ "${1:-}" == "--" ]]; then
  shift
fi
subcommand="${1:?usage: exomonad-run.sh <subcommand> [args...]}"
shift

source exomonad/scripts/exomonad-build.sh

exec "$PWD/target/debug/exomonad" "$subcommand" "$@"
