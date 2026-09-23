#!/usr/bin/env bash
# Build this checkout and start an Exomonad run with its matched local tools.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

if [[ "${1:-}" == "--" ]]; then
  shift
fi

exec exomonad/scripts/exomonad-run.sh init "$@"
