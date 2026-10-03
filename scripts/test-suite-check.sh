#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
python3 - <<'PY'
import json
import subprocess
from pathlib import Path
from scripts.test_source_ownership import registration_errors

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True))
errors = registration_errors(metadata, Path.cwd())
if errors:
    for error in errors:
        print(f"error: {error}")
    raise SystemExit(1)
print("Cargo suites register every test-bearing integration source")
PY
