#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--stdin" ) ]]; then
  echo "usage: $0 [--stdin]" >&2
  exit 2
fi
exec python3 scripts/test-suite-check.py "$@"
