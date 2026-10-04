#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -lt 1 || ( "$1" != "check" && "$1" != "update" ) ]]; then
  echo "usage: $0 check|update [COHORT...]" >&2
  exit 2
fi

# Prepared resources are native build products. Corpus checks share the resident
# compiler, without retaining a second checked-in artifact inventory.
exec scripts/prepared-corpus.sh "$@"
