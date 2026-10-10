#!/usr/bin/env bash
# Qualify the M1 production browser against one already frozen native bundle.
set -euo pipefail
descriptor="${1:?usage: run-m1-acceptance.sh DESCRIPTOR OUTPUT}"
output="${2:?fresh evidence directory required}"
[[ $# -eq 2 ]] || { echo 'error: expected DESCRIPTOR OUTPUT' >&2; exit 2; }
exec python3 "$(dirname -- "$descriptor")/qualification.py" run "$descriptor" \
  --cohort m1 --output "$output"
