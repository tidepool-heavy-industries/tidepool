#!/usr/bin/env bash
# Diagnose the exact frozen compiler/runtime/browser selection without building.
set -euo pipefail
descriptor="${1:?usage: toolchain-doctor.sh DESCRIPTOR}"
[[ $# -eq 1 ]] || { echo 'error: expected DESCRIPTOR' >&2; exit 2; }
owner="$(dirname -- "$descriptor")/qualification.py"
[[ -f "$owner" ]] || { echo "error: frozen qualification owner missing: $owner" >&2; exit 1; }
python3 "$owner" verify "$descriptor"
exec python3 "$owner" environment "$descriptor"
