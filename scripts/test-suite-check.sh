#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--stdin" ) ]]; then
  echo "usage: $0 [--stdin]" >&2
  exit 2
fi
python3 - "$@" <<'PY'
import json
import subprocess
import sys
from pathlib import Path
from scripts.test_source_ownership import registration_errors

if sys.argv[1:] == ["--stdin"]:
    metadata_text = sys.stdin.read()
else:
    metadata_text = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True)
metadata = json.loads(metadata_text)
errors = registration_errors(metadata, Path.cwd())
if errors:
    for error in errors:
        print(f"error: {error}")
    raise SystemExit(1)
print("Cargo suites register every test-bearing integration source")
PY
