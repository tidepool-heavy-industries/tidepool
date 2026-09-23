#!/usr/bin/env bash
# Build this checkout and start an Exomonad run with its matched local tools.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# `just` preserves its option separator as the first variadic recipe
# argument. It separates Just flags, not Exomonad's clap input.
if [[ "${1:-}" == "--" ]]; then
  shift
fi

source exomonad/scripts/exomonad-build.sh

exec "$PWD/target/debug/exomonad" init "$@"
