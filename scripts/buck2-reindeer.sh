#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${TIDEPOOL_REINDEER_SHELL:-} != ready ]]; then
  exec bash scripts/dev-shell.sh env TIDEPOOL_REINDEER_SHELL=ready bash scripts/buck2-reindeer.sh "$@"
fi
if [[ $# -gt 1 || (${1:-} != '' && ${1:-} != --check) ]]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi
python3 - "${1:-}" <<'PY'
from pathlib import Path
import os
import re
import shutil
import subprocess
import sys
import tempfile

root = Path.cwd()
dest = root / 'third-party/rust'
outputs = ['Cargo.toml', 'Cargo.lock', 'BUCK']
checking = sys.argv[1] == '--check'
with tempfile.TemporaryDirectory(prefix='tidepool-buck-deps-') as temporary:
    stage = Path(temporary)
    shutil.copy2(dest / 'reindeer.toml', stage / 'reindeer.toml')
    shutil.copy2(dest / 'empty.rs', stage / 'empty.rs')
    shutil.copytree(dest / 'fixups', stage / 'fixups')
    subprocess.run([
        sys.executable, 'scripts/buck2-dependencies.py', '--output-dir', str(stage)
    ], cwd=root, check=True)
    subprocess.run([
        'reindeer', '-c', 'reindeer.toml', 'buckify'
    ], cwd=stage, check=True)

    names = re.findall(r'^alias\(\n    name = "([^"]+)"', (stage / 'BUCK').read_text(), re.MULTILINE)
    if len(names) != len(set(names)):
        raise SystemExit('Versioned dependency aliases require explicit first-party mappings')
    staged_outputs = {name: (stage / name).read_bytes() for name in outputs}
    stage_prefix = os.fsencode(str(stage))
    leaked = [name for name, contents in staged_outputs.items() if stage_prefix in contents]
    if leaked:
        raise SystemExit('Staging path leaked into generated Buck inputs: ' + ', '.join(leaked))

    if checking:
        changed = [name for name in outputs if not (dest / name).exists() or (dest / name).read_bytes() != staged_outputs[name]]
        if changed:
            raise SystemExit('Stale Buck dependency inputs; regenerate: ' + ', '.join(changed))
    else:
        # Each replacement is atomic. If interrupted between files, the remaining
        # generated files are visible as ordinary reviewable work and a rerun repairs them.
        for name in outputs:
            replacement = dest / (name + '.tmp')
            try:
                replacement.write_bytes(staged_outputs[name])
                os.replace(replacement, dest / name)
            finally:
                replacement.unlink(missing_ok=True)
PY
