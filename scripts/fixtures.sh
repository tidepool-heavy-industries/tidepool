#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -ne 1 || ( "$1" != "check" && "$1" != "update" ) ]]; then
  echo "usage: $0 check|update" >&2
  exit 2
fi
scripts/prepared-corpus.sh
