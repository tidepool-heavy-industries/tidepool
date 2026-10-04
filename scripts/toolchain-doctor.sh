#!/usr/bin/env bash
# Diagnose the exact frozen compiler/runtime/browser selection without building.
set -euo pipefail
bundle="${1:?usage: toolchain-doctor.sh BUNDLE DESCRIPTOR}"
descriptor="${2:?qualification descriptor required}"
[[ $# -eq 2 ]] || { echo 'error: expected BUNDLE DESCRIPTOR' >&2; exit 2; }
owner="$bundle/share/exomonad/qualification.py"
[[ -f "$owner" ]] || { echo "error: frozen qualification owner missing: $owner" >&2; exit 1; }
python3 "$owner" verify "$descriptor"
exec python3 "$owner" environment "$descriptor"
