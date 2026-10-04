#!/usr/bin/env bash
# Qualify the M1 production browser against one already frozen native bundle.
set -euo pipefail
bundle="${1:?usage: run-m1-acceptance.sh BUNDLE DESCRIPTOR OUTPUT}"
descriptor="${2:?qualification descriptor required}"
output="${3:?fresh evidence directory required}"
[[ $# -eq 3 ]] || { echo 'error: expected BUNDLE DESCRIPTOR OUTPUT' >&2; exit 2; }
exec python3 "$bundle/share/exomonad/qualification.py" run "$descriptor" \
  --cohort m1 --output "$output"
