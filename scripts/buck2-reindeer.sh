#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${TIDEPOOL_REINDEER_SHELL:-} != ready ]]; then
  exec bash scripts/dev-shell.sh env TIDEPOOL_REINDEER_SHELL=ready bash scripts/buck2-reindeer.sh "$@"
fi
checking=0
dependency_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      if [[ $checking == 1 ]]; then
        echo "duplicate --check" >&2
        exit 2
      fi
      checking=1
      shift
      ;;
    --no-default-features|--features)
      if [[ $# -lt 2 ]]; then
        echo "missing value for $1" >&2
        exit 2
      fi
      dependency_args+=("$1" "$2")
      shift 2
      ;;
    *)
      echo "usage: $0 [--check] [--no-default-features PACKAGE] [--features PACKAGE=FEATURE[,FEATURE...]]" >&2
      exit 2
      ;;
  esac
done
python3 - "$checking" "${dependency_args[@]}" <<'PY'
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
checking = sys.argv[1] == '1'
dependency_args = sys.argv[2:]
with tempfile.TemporaryDirectory(prefix='tidepool-buck-deps-') as temporary:
    stage = Path(temporary)
    shutil.copy2(dest / 'reindeer.toml', stage / 'reindeer.toml')
    shutil.copy2(dest / 'empty.rs', stage / 'empty.rs')
    shutil.copytree(dest / 'fixups', stage / 'fixups')
    subprocess.run([
        sys.executable, 'scripts/buck2-dependencies.py', '--output-dir', str(stage), *dependency_args
    ], cwd=root, check=True)
    subprocess.run([
        'reindeer', '-c', 'reindeer.toml', 'buckify'
    ], cwd=stage, check=True)

    # Keep the matched harness source available to the embedded web action.
    # The generated Rust crate target remains private; this public filegroup
    # only forwards the exact locked checkout as a declared source input.
    buck = (stage / 'BUCK').read_text()
    harness_fetch = next(
        (
            (re.search(r'name = "([^"]+)"', stanza).group(1))
            for stanza in re.findall(r'git_fetch\(\n(.*?)\n\)', buck, re.DOTALL)
            if 'repo = "https://github.com/tidepool-heavy-industries/exomonad-harness.git"' in stanza
        ),
        None,
    )
    if harness_fetch:
        if 'load("@prelude//:rules.bzl", "filegroup")' not in buck:
            buck = buck.replace(
                'load("@prelude//rust:cargo_package.bzl", "cargo")',
                'load("@prelude//rust:cargo_package.bzl", "cargo")\n'
                'load("@prelude//:rules.bzl", "filegroup")',
                1,
            )
        buck += (
            '\nfilegroup(\n'
            '    name = "matched_harness_source",\n'
            f'    srcs = [":{harness_fetch}"],\n'
            '    visibility = ["PUBLIC"],\n'
            ')\n'
        )
        (stage / 'BUCK').write_text(buck)

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
