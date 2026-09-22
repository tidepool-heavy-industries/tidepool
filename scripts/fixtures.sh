#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -lt 1 || ( "$1" != "check" && "$1" != "update" ) ]]; then
  echo "usage: $0 check|update [COHORT...]" >&2
  exit 2
fi

# Rust tests include a small set of prepared artifacts directly. Check that
# their registered producers and schema are current before the heavier corpus
# operation, then repeat after an update so a stale blob cannot be published.
if [[ "$1" == "check" ]]; then
  python3 scripts/embedded-fixtures-check.py
fi
scripts/prepared-corpus.sh "$@"
python3 scripts/embedded-fixtures-check.py
