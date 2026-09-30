#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${TIDEPOOL_REINDEER_SHELL:-} != ready ]]; then
  exec bash scripts/dev-shell.sh env TIDEPOOL_REINDEER_SHELL=ready bash scripts/buck2-reindeer.sh "$@"
fi
checking=0
local_harness_source=0
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
    --local-harness-source)
      if [[ $local_harness_source == 1 ]]; then
        echo "duplicate --local-harness-source" >&2
        exit 2
      fi
      local_harness_source=1
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
      echo "usage: $0 [--check] [--local-harness-source] [--no-default-features PACKAGE] [--features PACKAGE=FEATURE[,FEATURE...]]" >&2
      exit 2
      ;;
  esac
done
python3 - "$checking" "$local_harness_source" "${dependency_args[@]}" <<'PY'
from pathlib import Path
import configparser
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

root = Path.cwd()
dest = root / 'third-party/rust'
outputs = ['Cargo.toml', 'Cargo.lock', 'BUCK']
checking = sys.argv[1] == '1'
local_harness_source = sys.argv[2] == '1'
dependency_args = sys.argv[3:]
HARNESS_REPO = 'https://github.com/tidepool-heavy-industries/exomonad-harness.git'

def current_harness_source_output():
    # Mirror scripts/dev-shell.sh's reduced committed source so this check is
    # independent of an inherited TIDEPOOL_DEV_SHELL and never captures the
    # Codex submodule or mounted build outputs.
    nix_inputs = ['flake.nix', 'flake.lock', 'rust-toolchain.toml', 'nix']
    if subprocess.run(['git', 'diff', '--quiet', 'HEAD', '--', *nix_inputs], cwd=root).returncode:
        raise SystemExit('Cannot verify matched harness source: commit Nix inputs before Reindeer generation')
    listing = subprocess.run(
        ['git', 'ls-tree', 'HEAD', '--', *nix_inputs], cwd=root,
        check=True, capture_output=True,
    ).stdout
    tree = subprocess.run(['git', 'mktree'], cwd=root, input=listing, check=True, capture_output=True).stdout.decode().strip()
    commit_env = os.environ | {
        'GIT_AUTHOR_DATE': '@0 +0000',
        'GIT_COMMITTER_DATE': '@0 +0000',
        'GIT_AUTHOR_NAME': 'dev-shell',
        'GIT_AUTHOR_EMAIL': 'dev-shell@invalid',
        'GIT_COMMITTER_NAME': 'dev-shell',
        'GIT_COMMITTER_EMAIL': 'dev-shell@invalid',
    }
    revision = subprocess.run(
        ['git', 'commit-tree', tree, '-m', 'dev-shell toolchain-input pin'],
        cwd=root, env=commit_env, check=True, capture_output=True, text=True,
    ).stdout.strip()
    ref = f'refs/tidepool/dev-shell/{revision}'
    if subprocess.run(['git', 'rev-parse', '-q', '--verify', ref], cwd=root, capture_output=True).returncode:
        raise SystemExit('Cannot verify matched harness source: enter the pinned Tidepool dev shell first')
    flake = f'git+file://{root}?rev={revision}'
    system = subprocess.run(
        ['nix', 'eval', '--raw', '--impure', '--expr', 'builtins.currentSystem'],
        cwd=root, check=True, capture_output=True, text=True,
    ).stdout.strip()
    return subprocess.run(
        ['nix', 'eval', '--raw', f'{flake}#packages.{system}.buck-matched-harness-source.outPath'],
        cwd=root, check=True, capture_output=True, text=True,
    ).stdout.strip()

def parse_harness_lock():
    try:
        cargo = tomllib.loads((root / 'Cargo.lock').read_text())
        packages = [p for p in cargo['package'] if p['name'] == 'harness']
        if len(packages) != 1:
            raise ValueError(f'expected exactly one locked harness package, found {len(packages)}')
        source = packages[0]['source']
        match = re.fullmatch(r'git\+' + re.escape(HARNESS_REPO) + r'\?rev=([0-9a-f]{40})#([0-9a-f]{40})', source)
        if not match or match.group(1) != match.group(2):
            raise ValueError('Cargo.lock harness source is not the canonical pinned Git revision')
        flake = json.loads((root / 'flake.lock').read_text())
        node = flake['nodes']['harnessWeb']
        for kind in ('original', 'locked'):
            pin = node[kind]
            if (pin.get('type'), pin.get('owner'), pin.get('repo'), pin.get('rev')) != (
                'github', 'tidepool-heavy-industries', 'exomonad-harness', match.group(1)
            ):
                raise ValueError(f'flake.lock harnessWeb {kind} pin does not match Cargo.lock')
        if node.get('flake') is not False:
            raise ValueError('flake.lock harnessWeb must be a non-flake source')
        return match.group(1)
    except (KeyError, OSError, ValueError, tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        raise SystemExit(f'Cannot select matched harness source: {error}')

def use_local_harness_source(buck):
    rev = parse_harness_lock()
    expected_source_path = current_harness_source_output()
    config = configparser.ConfigParser()
    try:
        with (root / '.buckconfig.local').open() as stream:
            config.read_file(stream)
        source_path = config['nix']['matched_harness_source'].strip()
    except (OSError, KeyError, configparser.Error) as error:
        raise SystemExit(f'Cannot select matched harness source: missing configured Nix source: {error}')
    if source_path != expected_source_path:
        raise SystemExit('Cannot select matched harness source: configured source is stale or differs from the current pinned Nix output')
    if not re.fullmatch(r'/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-tidepool-matched-harness-source', expected_source_path):
        raise SystemExit('Cannot select matched harness source: current flake output is not the declared immutable Nix source')

    stanzas = re.findall(r'git_fetch\(\n(.*?)\n\)', buck, re.DOTALL)
    matches = [stanza for stanza in stanzas if f'repo = "{HARNESS_REPO}"' in stanza]
    if len(matches) != 1:
        raise SystemExit(f'Cannot select matched harness source: expected one generated git_fetch candidate, found {len(matches)}')
    stanza = matches[0]
    name_match = re.search(r'name = "([^\"]+)"', stanza)
    rev_match = re.search(r'rev = "([^\"]+)"', stanza)
    if not name_match or not rev_match or rev_match.group(1) != rev:
        raise SystemExit('Cannot select matched harness source: generated git_fetch revision does not match Cargo.lock and flake.lock')
    fetch_name = name_match.group(1)
    if not fetch_name.endswith('.git') or not re.fullmatch(r'[A-Za-z0-9._+-]+\.git', fetch_name):
        raise SystemExit('Cannot select matched harness source: generated git_fetch target name is invalid')
    directory_name = fetch_name[:-4]
    if re.search(r'name = "' + re.escape(directory_name) + r'"', buck):
        raise SystemExit('Cannot select matched harness source: generated directory target name is already in use')
    if not re.search(r'crate_root = "' + re.escape(directory_name) + r'/crates/harness/src/lib\.rs"', buck):
        raise SystemExit('Cannot select matched harness source: generated harness crate root layout changed')
    if f'srcs = [":{fetch_name}"]' not in buck:
        raise SystemExit('Cannot select matched harness source: generated harness target does not reference its git_fetch source')
    if 'load("root//toolchains:tidepool.bzl", "nix_directory")' in buck:
        raise SystemExit('Cannot select matched harness source: generated BUCK already has a local source rule')

    rendered = (
        'nix_directory(\n'
        f'    name = "{directory_name}",\n'
        '    cp = read_root_config("nix", "coreutils") + "/cp",\n'
        '    store_path = read_root_config("nix", "matched_harness_source"),\n'
        '    visibility = [],\n'
        ')\n'
        'filegroup(\n'
        f'    name = "{fetch_name}",\n'
        f'    srcs = [":{directory_name}"],\n'
        '    visibility = [],\n'
        ')'
    )
    buck = buck.replace(f'git_fetch(\n{stanza}\n)', rendered, 1)
    buck = buck.replace(
        'load("@prelude//rust:cargo_package.bzl", "cargo")',
        'load("@prelude//rust:cargo_package.bzl", "cargo")\n'
        'load("root//toolchains:tidepool.bzl", "nix_directory")',
        1,
    )
    return buck, fetch_name
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
    if local_harness_source:
        buck, harness_fetch = use_local_harness_source(buck)
    else:
        harness_fetch = next(
            (
                re.search(r'name = "([^\"]+)"', stanza).group(1)
                for stanza in re.findall(r'git_fetch\(\n(.*?)\n\)', buck, re.DOTALL)
                if f'repo = "{HARNESS_REPO}"' in stanza
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
