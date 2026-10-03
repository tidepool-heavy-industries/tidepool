#!/usr/bin/env python3
"""Validate test-suite registration from Cargo metadata."""
import json
import subprocess
import sys
from pathlib import Path

from test_source_ownership import registration_errors


if sys.argv[1:] == ["--stdin"]:
    metadata_text = sys.stdin.read()
elif not sys.argv[1:]:
    metadata_text = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True)
else:
    raise SystemExit("usage: test-suite-check.py [--stdin]")

metadata = json.loads(metadata_text)
errors = registration_errors(metadata, Path.cwd())
if errors:
    for error in errors:
        print(f"error: {error}")
    raise SystemExit(1)
print("Cargo suites register every test-bearing integration source")
