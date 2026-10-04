#!/usr/bin/env bash
# Freeze declared native products at their final deployment path. Existing
# installations and resident hosts retain their current package bytes.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$repo_root/build/package/qualification.py" freeze "$@"
