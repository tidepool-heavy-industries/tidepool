#!/usr/bin/env bash
set -euo pipefail
PACKAGE_BIN=$(cd -- "$(dirname -- "$0")" && pwd)
PACKAGE_ROOT=$(cd -- "$PACKAGE_BIN/.." && pwd)
QUALIFIED_ENVIRONMENT=$("$PACKAGE_ROOT/share/exomonad/runtime-tools/bin/python3" -I \
  "$PACKAGE_ROOT/share/exomonad/qualification.py" environment \
  "$PACKAGE_ROOT/share/exomonad/qualification.json" --shell)
eval "$QUALIFIED_ENVIRONMENT"
exec "$PACKAGE_BIN/exomonad-unwrapped" "$@"
