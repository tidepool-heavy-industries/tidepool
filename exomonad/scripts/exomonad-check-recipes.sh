#!/usr/bin/env bash
# Each recipe uses the same verified frozen host and its production compiler owner.
set -euo pipefail
bundle="${1:?usage: exomonad-check-recipes.sh BUNDLE DESCRIPTOR REPORTS WORKSPACE [PARALLELISM]}"
descriptor="${2:?qualification descriptor required}"
reports="${3:?fresh report directory required}"
workspace="${4:?workspace required}"
parallelism="${5:-1}"
[[ $# -le 5 && "$parallelism" =~ ^[1-9][0-9]*$ ]] || { echo 'error: positive parallelism required' >&2; exit 2; }
[[ ! -e "$reports" ]] || { echo "error: report directory already exists: $reports" >&2; exit 2; }
mkdir -p "$reports"
exec python3 - "$bundle/share/exomonad/qualification.py" "$descriptor" "$reports" "$workspace" "$parallelism" <<'PY'
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import re
import subprocess
import sys
import tomllib
owner, descriptor, reports, workspace = map(Path, sys.argv[1:5])
parallelism = int(sys.argv[5])
with (workspace / ".exomonad/config.toml").open("rb") as stream:
    entries = tomllib.load(stream).get("haskell", {}).get("checks", [])
if not entries or any(not isinstance(entry, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9.]*", entry) for entry in entries):
    raise SystemExit("recipe checks must contain at least one valid Haskell entrypoint")
if len(entries) != len(set(entries)):
    raise SystemExit("recipe checks must name distinct entrypoints")
def run(entry):
    with (reports / (entry + ".log")).open("xb") as stream:
        result = subprocess.run([sys.executable, str(owner), "exec", "--report",
            str(reports / (entry + ".process.json")), str(descriptor), "--", "check",
            "--workspace", str(workspace), "--recipe", entry], stdout=stream, stderr=subprocess.STDOUT)
    print(f"{'passed' if result.returncode == 0 else 'FAILED'} {entry}: {reports / (entry + '.log')}", flush=True)
    return result.returncode
with ThreadPoolExecutor(max_workers=parallelism) as executor:
    statuses = list(executor.map(run, entries))
raise SystemExit(next((code for code in statuses if code), 0))
PY
